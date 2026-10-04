// Automation tests for automatic session creation: run with "Automation RunTests Rift.Network".

#include "Engine/Engine.h"
#include "Engine/GameInstance.h"
#include "IWebSocket.h"
#include "Misc/AutomationTest.h"
#include "RiftNetworkSubsystem.h"
#include "RiftProtocol.h"

#if WITH_DEV_AUTOMATION_TESTS

/** Stands in for the backend connection: records what the client sends, the test plays the server */
class FRiftFakeSocket : public IWebSocket
{
public:

	virtual void Connect() override {}
	virtual void Close(int32 Code = 1000, const FString& Reason = FString()) override {}
	virtual bool IsConnected() override { return true; }
	virtual void Send(const FString& Data) override { Sent.Add(Data); }
	virtual void Send(const void* Data, SIZE_T Size, bool bIsBinary = false) override {}
	virtual void SetTextMessageMemoryLimit(uint64 TextMessageMemoryLimit) override {}

	virtual FWebSocketConnectedEvent& OnConnected() override { return Connected; }
	virtual FWebSocketConnectionErrorEvent& OnConnectionError() override { return ConnectionError; }
	virtual FWebSocketClosedEvent& OnClosed() override { return Closed; }
	virtual FWebSocketMessageEvent& OnMessage() override { return Message; }
	virtual FWebSocketBinaryMessageEvent& OnBinaryMessage() override { return BinaryMessage; }
	virtual FWebSocketRawMessageEvent& OnRawMessage() override { return RawMessage; }
	virtual FWebSocketMessageSentEvent& OnMessageSent() override { return MessageSent; }

	/** Decodes the sent frames of one message type, oldest first */
	TArray<FRiftEnvelope> SentOfType(const TCHAR* MessageType) const
	{
		TArray<FRiftEnvelope> Result;

		for (const FString& Frame : Sent)
		{
			FRiftEnvelope Envelope;
			FString ParseError;

			if (RiftProtocol::ParseEnvelope(Frame, Envelope, ParseError) && Envelope.MessageType.Equals(MessageType, ESearchCase::CaseSensitive))
			{
				Result.Add(Envelope);
			}
		}

		return Result;
	}

	TArray<FString> Sent;

	FWebSocketConnectedEvent Connected;
	FWebSocketConnectionErrorEvent ConnectionError;
	FWebSocketClosedEvent Closed;
	FWebSocketMessageEvent Message;
	FWebSocketBinaryMessageEvent BinaryMessage;
	FWebSocketRawMessageEvent RawMessage;
	FWebSocketMessageSentEvent MessageSent;
};

/** The one door into the subsystem's private parts, a friend of URiftNetworkSubsystem */
struct FRiftNetworkTestAccess
{
	static void SetSocketFactory(URiftNetworkSubsystem& Network, TFunction<TSharedPtr<IWebSocket>()> Factory)
	{
		Network.SocketFactory = MoveTemp(Factory);
	}
};

namespace
{
	/** A network subsystem that was never initialized, so it only connects when the test says so */
	struct FRiftNetworkFixture
	{
		FRiftNetworkFixture()
		{
			Network = NewObject<URiftNetworkSubsystem>(NewObject<UGameInstance>(GEngine));

			FRiftNetworkTestAccess::SetSocketFactory(*Network, [this]() -> TSharedPtr<IWebSocket>
			{
				Sockets.Add(MakeShared<FRiftFakeSocket>());
				return Sockets.Last();
			});
		}

		/** The socket of the current connection */
		FRiftFakeSocket& Socket() const { return *Sockets.Last(); }

		/** Connects and lets the socket open, hello goes out */
		void Open()
		{
			Network->Connect();
			Socket().Connected.Broadcast();
		}

		void Receive(const TCHAR* MessageType, const FString& PayloadJson, const FString& SessionId = FString(), const FString& ReplyTo = FString())
		{
			const FString Session = SessionId.IsEmpty() ? FString(TEXT("null")) : FString::Printf(TEXT("\"%s\""), *SessionId);

			Socket().Message.Broadcast(FString::Printf(
				TEXT("{\"protocol_version\":1,\"message_id\":\"%s\",\"message_type\":\"%s\",\"timestamp\":\"2026-10-04T00:00:00Z\",\"session_id\":%s,\"reply_to\":\"%s\",\"payload\":%s}"),
				*RiftProtocol::NewId(), MessageType, *Session, *ReplyTo, *PayloadJson));
		}

		void ReceiveHelloAck()
		{
			Receive(TEXT("hello_ack"), TEXT("{\"protocol_version\":1,\"server\":\"fake\",\"server_version\":\"0\",\"connection_id\":\"c\"}"));
		}

		void ReceiveSessionCreated(const FString& SessionId)
		{
			Receive(TEXT("session_created"), TEXT("{}"), SessionId);
		}

		/** Connects, handshakes and answers the automatic create_session */
		void OpenWithSession(const FString& SessionId)
		{
			Open();
			ReceiveHelloAck();
			ReceiveSessionCreated(SessionId);
		}

		int32 CreateSessionCount() const { return Socket().SentOfType(RiftProtocol::MessageType::CreateSession).Num(); }

		URiftNetworkSubsystem* Network = nullptr;

		TArray<TSharedPtr<FRiftFakeSocket>> Sockets;
	};

	const EAutomationTestFlags RiftNetworkTestFlags = EAutomationTestFlags_ApplicationContextMask | EAutomationTestFlags::EngineFilter;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FRiftAutoSessionConnectTest, "Rift.Network.AutoSession.ConnectionCreatesSession", RiftNetworkTestFlags)

bool FRiftAutoSessionConnectTest::RunTest(const FString& Parameters)
{
	FRiftNetworkFixture Fixture;

	Fixture.Open();

	TestEqual(TEXT("hello is sent first"), Fixture.Socket().SentOfType(RiftProtocol::MessageType::Hello).Num(), 1);
	TestEqual(TEXT("no create_session before hello_ack"), Fixture.CreateSessionCount(), 0);

	Fixture.ReceiveHelloAck();

	TestTrue(TEXT("connection is ready"), Fixture.Network->IsConnected());
	TestEqual(TEXT("hello_ack triggers one create_session"), Fixture.CreateSessionCount(), 1);
	TestTrue(TEXT("session is pending"), Fixture.Network->IsSessionPending());
	TestTrue(TEXT("no session id yet"), Fixture.Network->GetSessionId().IsEmpty());

	const TArray<FRiftEnvelope> Requests = Fixture.Socket().SentOfType(RiftProtocol::MessageType::CreateSession);

	if (Requests.Num() == 1)
	{
		TestTrue(TEXT("create_session carries no session id"), Requests[0].SessionId.IsEmpty());
		TestEqual(TEXT("create_session payload is empty"), Requests[0].Payload->Values.Num(), 0);
	}

	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FRiftAutoSessionStoreTest, "Rift.Network.AutoSession.SessionCreatedStoresSession", RiftNetworkTestFlags)

bool FRiftAutoSessionStoreTest::RunTest(const FString& Parameters)
{
	FRiftNetworkFixture Fixture;

	Fixture.OpenWithSession(TEXT("session-1"));

	TestEqual(TEXT("session id is stored"), Fixture.Network->GetSessionId(), FString(TEXT("session-1")));
	TestFalse(TEXT("request is no longer pending"), Fixture.Network->IsSessionPending());

	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FRiftAutoSessionDuplicateTest, "Rift.Network.AutoSession.NoDuplicateSessions", RiftNetworkTestFlags)

bool FRiftAutoSessionDuplicateTest::RunTest(const FString& Parameters)
{
	FRiftNetworkFixture Fixture;

	Fixture.Open();

	// repeated callbacks while the request is on its way
	Fixture.ReceiveHelloAck();
	Fixture.ReceiveHelloAck();
	Fixture.Socket().Connected.Broadcast();
	Fixture.ReceiveHelloAck();
	Fixture.Network->Connect();

	TestFalse(TEXT("EnsureSession sends nothing while pending"), Fixture.Network->EnsureSession());
	TestEqual(TEXT("one create_session while pending"), Fixture.CreateSessionCount(), 1);

	Fixture.ReceiveSessionCreated(TEXT("session-1"));

	// and again once the session exists
	Fixture.ReceiveHelloAck();

	TestFalse(TEXT("EnsureSession sends nothing with a session"), Fixture.Network->EnsureSession());
	TestEqual(TEXT("still one create_session"), Fixture.CreateSessionCount(), 1);
	TestEqual(TEXT("still one socket"), Fixture.Sockets.Num(), 1);
	TestEqual(TEXT("session is unchanged"), Fixture.Network->GetSessionId(), FString(TEXT("session-1")));

	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FRiftAutoSessionDisconnectTest, "Rift.Network.AutoSession.DisconnectClearsSession", RiftNetworkTestFlags)

bool FRiftAutoSessionDisconnectTest::RunTest(const FString& Parameters)
{
	AddExpectedError(TEXT("Rift backend connection closed"), EAutomationExpectedErrorFlags::Contains, 2);
	AddExpectedError(TEXT("Cannot send player_action 'speak': no session"), EAutomationExpectedErrorFlags::Contains, 1);

	// the backend goes away with a session in use
	{
		FRiftNetworkFixture Fixture;

		Fixture.OpenWithSession(TEXT("session-1"));
		Fixture.Socket().Closed.Broadcast(1006, TEXT("backend stopped"), false);

		TestEqual(TEXT("state is disconnected"), Fixture.Network->GetConnectionState(), ERiftConnectionState::Disconnected);
		TestTrue(TEXT("session id is cleared"), Fixture.Network->GetSessionId().IsEmpty());
		TestFalse(TEXT("player_action is refused without a session"), Fixture.Network->SendPlayerAction(TEXT("speak"), TEXT("hank_schrader"), TEXT("hello")));
	}

	// the backend goes away before it answered create_session
	{
		FRiftNetworkFixture Fixture;

		Fixture.Open();
		Fixture.ReceiveHelloAck();
		Fixture.Socket().Closed.Broadcast(1006, TEXT("backend stopped"), false);

		TestFalse(TEXT("pending request is dropped"), Fixture.Network->IsSessionPending());
		TestTrue(TEXT("no session id"), Fixture.Network->GetSessionId().IsEmpty());
	}

	// closing from the client side ends the session too
	{
		FRiftNetworkFixture Fixture;

		Fixture.OpenWithSession(TEXT("session-1"));
		Fixture.Network->Disconnect();

		TestTrue(TEXT("Disconnect clears the session id"), Fixture.Network->GetSessionId().IsEmpty());
	}

	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FRiftAutoSessionReconnectTest, "Rift.Network.AutoSession.ReconnectCreatesFreshSession", RiftNetworkTestFlags)

bool FRiftAutoSessionReconnectTest::RunTest(const FString& Parameters)
{
	AddExpectedError(TEXT("Rift backend connection closed"), EAutomationExpectedErrorFlags::Contains, 1);

	FRiftNetworkFixture Fixture;

	Fixture.OpenWithSession(TEXT("session-1"));
	Fixture.Socket().Closed.Broadcast(1006, TEXT("backend stopped"), false);

	Fixture.Open();

	TestEqual(TEXT("reconnect uses a new socket"), Fixture.Sockets.Num(), 2);
	TestEqual(TEXT("no create_session before the new hello_ack"), Fixture.CreateSessionCount(), 0);

	Fixture.ReceiveHelloAck();

	TestEqual(TEXT("new connection asks for one session"), Fixture.CreateSessionCount(), 1);

	Fixture.ReceiveSessionCreated(TEXT("session-2"));

	TestEqual(TEXT("fresh session id is stored"), Fixture.Network->GetSessionId(), FString(TEXT("session-2")));

	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FRiftAutoSessionActionTest, "Rift.Network.AutoSession.PlayerActionAfterSession", RiftNetworkTestFlags)

bool FRiftAutoSessionActionTest::RunTest(const FString& Parameters)
{
	FRiftNetworkFixture Fixture;

	Fixture.OpenWithSession(TEXT("session-1"));

	TestTrue(TEXT("speak is sent"), Fixture.Network->SendPlayerAction(TEXT("speak"), TEXT("hank_schrader"), TEXT("hello there")));
	TestTrue(TEXT("interact is sent"), Fixture.Network->SendPlayerAction(TEXT("interact"), TEXT("burner_phone")));

	const TArray<FRiftEnvelope> Actions = Fixture.Socket().SentOfType(RiftProtocol::MessageType::PlayerAction);

	TestEqual(TEXT("two player_action frames"), Actions.Num(), 2);

	if (Actions.Num() == 2)
	{
		const FJsonObject& Speak = *Actions[0].Payload;

		TestEqual(TEXT("envelope session id"), Actions[0].SessionId, FString(TEXT("session-1")));
		TestEqual(TEXT("action session id"), Speak.GetStringField(TEXT("session_id")), FString(TEXT("session-1")));
		TestEqual(TEXT("action type"), Speak.GetStringField(TEXT("action_type")), FString(TEXT("speak")));
		TestEqual(TEXT("target"), Speak.GetStringField(TEXT("target")), FString(TEXT("hank_schrader")));
		TestEqual(TEXT("content"), Speak.GetStringField(TEXT("content")), FString(TEXT("hello there")));

		TestEqual(TEXT("second envelope session id"), Actions[1].SessionId, FString(TEXT("session-1")));
		TestEqual(TEXT("second action type"), Actions[1].Payload->GetStringField(TEXT("action_type")), FString(TEXT("interact")));
	}

	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FRiftAutoSessionFailureTest, "Rift.Network.AutoSession.FailureIsLoggedNotFatal", RiftNetworkTestFlags)

bool FRiftAutoSessionFailureTest::RunTest(const FString& Parameters)
{
	AddExpectedError(TEXT("Rift backend error 'internal'"), EAutomationExpectedErrorFlags::Contains, 1);
	AddExpectedError(TEXT("Rift session creation failed"), EAutomationExpectedErrorFlags::Contains, 1);
	AddExpectedError(TEXT("Rift backend error 'session_not_found'"), EAutomationExpectedErrorFlags::Contains, 1);

	FRiftNetworkFixture Fixture;

	Fixture.Open();
	Fixture.ReceiveHelloAck();

	const TArray<FRiftEnvelope> Requests = Fixture.Socket().SentOfType(RiftProtocol::MessageType::CreateSession);

	if (!TestEqual(TEXT("one create_session"), Requests.Num(), 1))
	{
		return true;
	}

	// the backend refuses the request
	Fixture.Receive(TEXT("error"), TEXT("{\"code\":\"internal\",\"message\":\"no session for you\"}"), FString(), Requests[0].MessageId);

	TestFalse(TEXT("request is no longer pending"), Fixture.Network->IsSessionPending());
	TestTrue(TEXT("no session id"), Fixture.Network->GetSessionId().IsEmpty());
	TestTrue(TEXT("connection survives"), Fixture.Network->IsConnected());
	TestEqual(TEXT("a refused request is not retried by itself"), Fixture.CreateSessionCount(), 1);

	// a later request still works
	TestTrue(TEXT("EnsureSession asks again"), Fixture.Network->EnsureSession());
	Fixture.ReceiveSessionCreated(TEXT("session-1"));
	TestEqual(TEXT("session id is stored"), Fixture.Network->GetSessionId(), FString(TEXT("session-1")));

	// the backend lost the session: a fresh one is requested
	Fixture.Receive(TEXT("error"), TEXT("{\"code\":\"session_not_found\",\"message\":\"gone\"}"), FString(), TEXT("some-action"));

	TestTrue(TEXT("lost session is dropped"), Fixture.Network->GetSessionId().IsEmpty());
	TestEqual(TEXT("a lost session is replaced"), Fixture.CreateSessionCount(), 3);

	return true;
}

#endif
