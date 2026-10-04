// Persistent WebSocket connection between the game and the Rift backend.

#include "RiftNetworkSubsystem.h"

#include "Dom/JsonObject.h"
#include "IWebSocket.h"
#include "Misc/CommandLine.h"
#include "Misc/Parse.h"
#include "RiftProtocol.h"
#include "WebSocketsModule.h"

namespace
{
	const TCHAR* const ClientName = TEXT("rift-unreal");
	const TCHAR* const ClientVersion = TEXT("0.1.0");

	const TCHAR* const TestActionType = TEXT("interact");
	const TCHAR* const TestActionTarget = TEXT("test_door");

	/** Message types are compared exactly, FString's == ignores case */
	bool IsType(const FString& MessageType, const TCHAR* Expected)
	{
		return MessageType.Equals(Expected, ESearchCase::CaseSensitive);
	}
}

void URiftNetworkSubsystem::Initialize(FSubsystemCollectionBase& Collection)
{
	Super::Initialize(Collection);

	// -RiftBackendUrl=ws://host:port/ws overrides the configured endpoint for this run
	FParse::Value(FCommandLine::Get(), TEXT("RiftBackendUrl="), BackendUrl);

	if (bAutoConnect)
	{
		Connect();
	}
}

void URiftNetworkSubsystem::Deinitialize()
{
	CloseSocket();
	State = ERiftConnectionState::Disconnected;

	Super::Deinitialize();
}

void URiftNetworkSubsystem::Connect()
{
	if (State != ERiftConnectionState::Disconnected)
	{
		return;
	}

	// a finished socket from an earlier attempt may still be held
	CloseSocket();

	UE_LOG(LogRiftNet, Log, TEXT("Connecting to Rift backend at %s"), *BackendUrl);

	Socket = FWebSocketsModule::Get().CreateWebSocket(BackendUrl);

	// bound weakly: the socket can outlive this object without calling into it
	Socket->OnConnected().AddUObject(this, &URiftNetworkSubsystem::HandleSocketConnected);
	Socket->OnConnectionError().AddUObject(this, &URiftNetworkSubsystem::HandleSocketConnectionError);
	Socket->OnClosed().AddUObject(this, &URiftNetworkSubsystem::HandleSocketClosed);
	Socket->OnMessage().AddUObject(this, &URiftNetworkSubsystem::HandleSocketMessage);

	// set before Connect: a refused URL reports its error from inside the call
	State = ERiftConnectionState::Connecting;
	bSocketActive = true;

	Socket->Connect();
}

void URiftNetworkSubsystem::Disconnect()
{
	if (State == ERiftConnectionState::Disconnected)
	{
		return;
	}

	CloseSocket();
	EnterDisconnected(TEXT("closed by client"));
}

bool URiftNetworkSubsystem::CreateSession()
{
	return SendEnvelope(RiftProtocol::MessageType::CreateSession, nullptr);
}

bool URiftNetworkSubsystem::SendTestAction()
{
	return SendPlayerAction(TestActionType, TestActionTarget);
}

bool URiftNetworkSubsystem::SendPing(const FString& Nonce)
{
	return SendEnvelope(RiftProtocol::MessageType::Ping, RiftProtocol::MakePing(Nonce));
}

bool URiftNetworkSubsystem::SendPlayerAction(const FString& ActionType, const FString& Target, const FString& Content, const FString& ActionId)
{
	if (SessionId.IsEmpty())
	{
		UE_LOG(LogRiftNet, Warning, TEXT("Cannot send player_action '%s': no session, create one first"), *ActionType);
		return false;
	}

	const FString Id = ActionId.IsEmpty() ? RiftProtocol::NewId() : ActionId;

	// the backend requires the session id on both the envelope and the action
	return SendEnvelope(
		RiftProtocol::MessageType::PlayerAction,
		RiftProtocol::MakePlayerAction(Id, SessionId, ActionType, Target, Content),
		true);
}

bool URiftNetworkSubsystem::SendEnvelope(const FString& MessageType, const TSharedPtr<FJsonObject>& Payload, bool bWithSession)
{
	const bool bSocketOpen = Socket.IsValid() && (State == ERiftConnectionState::Handshaking || State == ERiftConnectionState::Ready);

	if (!bSocketOpen)
	{
		UE_LOG(LogRiftNet, Warning, TEXT("Cannot send %s: not connected to the Rift backend"), *MessageType);
		return false;
	}

	// until hello_ack arrives the backend only accepts hello and ping
	const bool bAllowedBeforeHandshake = IsType(MessageType, RiftProtocol::MessageType::Hello) || IsType(MessageType, RiftProtocol::MessageType::Ping);

	if (State != ERiftConnectionState::Ready && !bAllowedBeforeHandshake)
	{
		UE_LOG(LogRiftNet, Warning, TEXT("Cannot send %s: handshake with the Rift backend is not finished"), *MessageType);
		return false;
	}

	FString MessageId;
	const FString Text = RiftProtocol::BuildEnvelope(MessageType, bWithSession ? SessionId : FString(), Payload, &MessageId);

	Socket->Send(Text);

	UE_LOG(LogRiftNet, Verbose, TEXT("-> %s (%s)"), *MessageType, *MessageId);
	return true;
}

void URiftNetworkSubsystem::HandleSocketConnected()
{
	State = ERiftConnectionState::Handshaking;

	// hello must be the first message on a connection
	SendEnvelope(RiftProtocol::MessageType::Hello, RiftProtocol::MakeHello(ClientName, ClientVersion));
}

void URiftNetworkSubsystem::HandleSocketConnectionError(const FString& Error)
{
	bSocketActive = false;

	UE_LOG(LogRiftNet, Warning, TEXT("Rift backend unavailable at %s: %s"), *BackendUrl, *Error);

	EnterDisconnected(FString::Printf(TEXT("connection error: %s"), *Error));
}

void URiftNetworkSubsystem::HandleSocketClosed(int32 StatusCode, const FString& Reason, bool bWasClean)
{
	bSocketActive = false;

	UE_LOG(LogRiftNet, Warning, TEXT("Rift backend connection closed (code %d, %s): %s"), StatusCode, bWasClean ? TEXT("clean") : TEXT("unclean"), *Reason);

	EnterDisconnected(FString::Printf(TEXT("connection closed: %s"), *Reason));
}

void URiftNetworkSubsystem::HandleSocketMessage(const FString& Message)
{
	FRiftEnvelope Envelope;
	FString ParseError;

	if (!RiftProtocol::ParseEnvelope(Message, Envelope, ParseError))
	{
		// a bad frame is dropped, the connection stays usable
		UE_LOG(LogRiftNet, Warning, TEXT("Dropped message from Rift backend: %s"), *ParseError);
		return;
	}

	UE_LOG(LogRiftNet, Verbose, TEXT("<- %s (reply to %s)"), *Envelope.MessageType, *Envelope.ReplyTo);

	if (IsType(Envelope.MessageType, RiftProtocol::MessageType::WorldEvent))
	{
		HandleWorldEvent(Envelope);
	}
	else if (IsType(Envelope.MessageType, RiftProtocol::MessageType::Error))
	{
		HandleError(Envelope);
	}
	else if (IsType(Envelope.MessageType, RiftProtocol::MessageType::Pong))
	{
		HandlePong(Envelope);
	}
	else if (IsType(Envelope.MessageType, RiftProtocol::MessageType::SessionCreated))
	{
		HandleSessionCreated(Envelope);
	}
	else if (IsType(Envelope.MessageType, RiftProtocol::MessageType::HelloAck))
	{
		HandleHelloAck(Envelope);
	}
	else
	{
		// V1 may add message types, an older client ignores them
		UE_LOG(LogRiftNet, Log, TEXT("Ignoring unknown message_type '%s'"), *Envelope.MessageType);
	}
}

void URiftNetworkSubsystem::HandleHelloAck(const FRiftEnvelope& Envelope)
{
	int32 ServerProtocol = 0;
	Envelope.Payload->TryGetNumberField(TEXT("protocol_version"), ServerProtocol);

	if (ServerProtocol != RiftProtocol::Version)
	{
		UE_LOG(LogRiftNet, Error, TEXT("Rift backend speaks protocol %d, this client speaks %d. Disconnecting."), ServerProtocol, RiftProtocol::Version);

		CloseSocket();
		EnterDisconnected(TEXT("protocol version mismatch"));
		return;
	}

	FString Server;
	FString ServerVersion;
	FString ConnectionId;
	Envelope.Payload->TryGetStringField(TEXT("server"), Server);
	Envelope.Payload->TryGetStringField(TEXT("server_version"), ServerVersion);
	Envelope.Payload->TryGetStringField(TEXT("connection_id"), ConnectionId);

	State = ERiftConnectionState::Ready;

	UE_LOG(LogRiftNet, Log, TEXT("hello_ack from %s %s, connection %s"), *Server, *ServerVersion, *ConnectionId);

	OnReady.Broadcast();
}

void URiftNetworkSubsystem::HandlePong(const FRiftEnvelope& Envelope)
{
	FString Nonce;
	Envelope.Payload->TryGetStringField(TEXT("nonce"), Nonce);

	UE_LOG(LogRiftNet, Log, TEXT("pong (nonce %s)"), *Nonce);

	OnPong.Broadcast(Nonce);
}

void URiftNetworkSubsystem::HandleSessionCreated(const FRiftEnvelope& Envelope)
{
	if (Envelope.SessionId.IsEmpty())
	{
		UE_LOG(LogRiftNet, Warning, TEXT("Ignoring session_created without a session_id"));
		return;
	}

	SessionId = Envelope.SessionId;

	UE_LOG(LogRiftNet, Log, TEXT("session_created: %s"), *SessionId);

	OnSessionCreated.Broadcast(SessionId);
}

void URiftNetworkSubsystem::HandleWorldEvent(const FRiftEnvelope& Envelope)
{
	const FJsonObject& Payload = *Envelope.Payload;

	FRiftWorldEvent Event;
	Payload.TryGetStringField(TEXT("event_id"), Event.EventId);
	Payload.TryGetStringField(TEXT("session_id"), Event.SessionId);
	Payload.TryGetNumberField(TEXT("sequence"), Event.Sequence);
	Payload.TryGetStringField(TEXT("event_type"), Event.EventType);
	Payload.TryGetStringField(TEXT("target"), Event.Target);
	Payload.TryGetStringField(TEXT("timestamp"), Event.Timestamp);

	const TSharedPtr<FJsonObject>* EventPayload = nullptr;

	if (Payload.TryGetObjectField(TEXT("payload"), EventPayload) && EventPayload)
	{
		Event.PayloadJson = RiftProtocol::ToJson(*EventPayload);
	}

	if (Event.EventType.IsEmpty())
	{
		UE_LOG(LogRiftNet, Warning, TEXT("Ignoring world_event without an event_type"));
		return;
	}

	UE_LOG(LogRiftNet, Log, TEXT("world_event #%lld: %s -> %s %s"), Event.Sequence, *Event.EventType, *Event.Target, *Event.PayloadJson);

	// hand the event to gameplay, nothing here changes the world
	OnWorldEvent.Broadcast(Event);
}

void URiftNetworkSubsystem::HandleError(const FRiftEnvelope& Envelope)
{
	FString Code;
	FString Message;
	Envelope.Payload->TryGetStringField(TEXT("code"), Code);
	Envelope.Payload->TryGetStringField(TEXT("message"), Message);

	UE_LOG(LogRiftNet, Warning, TEXT("Rift backend error '%s': %s (reply to %s)"), *Code, *Message, *Envelope.ReplyTo);

	// sessions live in backend memory: after a backend restart ours is gone
	if (IsType(Code, TEXT("session_not_found")))
	{
		SessionId.Reset();
	}

	OnBackendError.Broadcast(Code, Message);
}

void URiftNetworkSubsystem::CloseSocket()
{
	if (!Socket.IsValid())
	{
		return;
	}

	// unbind first so the close notification does not come back to us
	Socket->OnConnected().RemoveAll(this);
	Socket->OnConnectionError().RemoveAll(this);
	Socket->OnClosed().RemoveAll(this);
	Socket->OnMessage().RemoveAll(this);

	if (bSocketActive)
	{
		Socket->Close();
		bSocketActive = false;
	}

	Socket.Reset();
}

void URiftNetworkSubsystem::EnterDisconnected(const FString& Reason)
{
	State = ERiftConnectionState::Disconnected;

	OnDisconnected.Broadcast(Reason);
}
