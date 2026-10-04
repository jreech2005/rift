// Persistent WebSocket connection between the game and the Rift backend.

#pragma once

#include "CoreMinimal.h"
#include "Subsystems/GameInstanceSubsystem.h"
#include "RiftNetworkSubsystem.generated.h"

class FJsonObject;
class IWebSocket;
struct FRiftEnvelope;

/** Where the backend connection currently stands */
UENUM(BlueprintType)
enum class ERiftConnectionState : uint8
{
	Disconnected,

	/** The socket is opening */
	Connecting,

	/** The socket is open and hello was sent, waiting for hello_ack */
	Handshaking,

	/** hello_ack received, messages may be sent */
	Ready
};

/**
 *  Payload of a world_event message.
 *  A validated, structured outcome decided by the backend, for the game to present.
 */
USTRUCT(BlueprintType)
struct FRiftWorldEvent
{
	GENERATED_BODY()

	UPROPERTY(BlueprintReadOnly, Category="Rift|Network")
	FString EventId;

	UPROPERTY(BlueprintReadOnly, Category="Rift|Network")
	FString SessionId;

	/** Increases by one per event in a session, starting at 1 */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Network")
	int64 Sequence = 0;

	/** For example interaction_acknowledged. Kept as text because V1 may add event types */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Network")
	FString EventType;

	/** Empty when the event has no target */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Network")
	FString Target;

	/** The event specific payload object, as JSON text */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Network")
	FString PayloadJson;

	UPROPERTY(BlueprintReadOnly, Category="Rift|Network")
	FString Timestamp;
};

DECLARE_DYNAMIC_MULTICAST_DELEGATE(FRiftReadyDelegate);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FRiftDisconnectedDelegate, const FString&, Reason);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FRiftPongDelegate, const FString&, Nonce);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FRiftSessionCreatedDelegate, const FString&, SessionId);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FRiftWorldEventDelegate, const FRiftWorldEvent&, Event);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FRiftBackendErrorDelegate, const FString&, Code, const FString&, Message);

/**
 *  Owns the one WebSocket to the Rift backend and speaks protocol V1 (docs/PROTOCOL.md).
 *  Lives as long as the game instance. Event driven: socket callbacks arrive on the game thread,
 *  nothing is polled. Received world events are only broadcast, this class never touches the world.
 */
UCLASS(Config=Game)
class RIFT_API URiftNetworkSubsystem : public UGameInstanceSubsystem
{
	GENERATED_BODY()

public:

	/** Reads the URL override and connects if configured to */
	virtual void Initialize(FSubsystemCollectionBase& Collection) override;

	/** Closes the socket */
	virtual void Deinitialize() override;

	/** Opens the socket and sends hello. Does nothing while already connecting or connected */
	UFUNCTION(BlueprintCallable, Category="Rift|Network")
	void Connect();

	/** Closes the socket. The session ends with the connection, the next connection gets a fresh one */
	UFUNCTION(BlueprintCallable, Category="Rift|Network")
	void Disconnect();

	/** Returns true once the backend acknowledged hello */
	UFUNCTION(BlueprintPure, Category="Rift|Network")
	bool IsConnected() const { return State == ERiftConnectionState::Ready; }

	UFUNCTION(BlueprintPure, Category="Rift|Network")
	ERiftConnectionState GetConnectionState() const { return State; }

	/** Returns the current session id, empty until session_created arrives */
	UFUNCTION(BlueprintPure, Category="Rift|Network")
	FString GetSessionId() const { return SessionId; }

	/** Asks the backend for a new game session. The id arrives through OnSessionCreated */
	UFUNCTION(BlueprintCallable, Category="Rift|Network")
	bool CreateSession();

	/**
	 *  Asks for a session unless one exists or a request is already on its way.
	 *  Called on every hello_ack, safe to call from anywhere at any time.
	 *  @return true if a create_session was sent by this call
	 */
	UFUNCTION(BlueprintCallable, Category="Rift|Network")
	bool EnsureSession();

	/** Returns true while a create_session is waiting for its session_created */
	UFUNCTION(BlueprintPure, Category="Rift|Network")
	bool IsSessionPending() const { return bSessionPending; }

	/** Sends the interact / test_door action used to prove the action path */
	UFUNCTION(BlueprintCallable, Category="Rift|Network")
	bool SendTestAction();

	/** Sends a ping. The backend echoes the nonce through OnPong */
	bool SendPing(const FString& Nonce = FString());

	/**
	 *  Sends a player_action for the current session.
	 *  @param ActionId	leave empty for a new id. The backend rejects an id it already applied
	 *  @return false if there is no session or the connection is not ready
	 */
	bool SendPlayerAction(const FString& ActionType, const FString& Target, const FString& Content = FString(), const FString& ActionId = FString());

	/**
	 *  Wraps a payload in a V1 envelope and sends it.
	 *  @param bWithSession	puts the current session id on the envelope instead of null
	 *  @param OutMessageId	optionally receives the message_id of the sent envelope
	 *  @return false if the connection cannot carry the message yet
	 */
	bool SendEnvelope(const FString& MessageType, const TSharedPtr<FJsonObject>& Payload, bool bWithSession = false, FString* OutMessageId = nullptr);

	const FString& GetBackendUrl() const { return BackendUrl; }

	/** hello_ack received, the connection is usable */
	UPROPERTY(BlueprintAssignable, Category="Rift|Network")
	FRiftReadyDelegate OnReady;

	/** The connection failed, was closed, or was closed by Disconnect */
	UPROPERTY(BlueprintAssignable, Category="Rift|Network")
	FRiftDisconnectedDelegate OnDisconnected;

	UPROPERTY(BlueprintAssignable, Category="Rift|Network")
	FRiftPongDelegate OnPong;

	UPROPERTY(BlueprintAssignable, Category="Rift|Network")
	FRiftSessionCreatedDelegate OnSessionCreated;

	/** A validated world event arrived. Present it, do not treat it as a request */
	UPROPERTY(BlueprintAssignable, Category="Rift|Network")
	FRiftWorldEventDelegate OnWorldEvent;

	/** The backend answered with an error message */
	UPROPERTY(BlueprintAssignable, Category="Rift|Network")
	FRiftBackendErrorDelegate OnBackendError;

protected:

	/**
	 *  Backend WebSocket endpoint. The only place the URL is defined.
	 *  Override in DefaultGame.ini under [/Script/Rift.RiftNetworkSubsystem] or with -RiftBackendUrl=
	 */
	UPROPERTY(Config)
	FString BackendUrl = TEXT("ws://127.0.0.1:3000/ws");

	/** If true, connects as soon as the game instance starts */
	UPROPERTY(Config)
	bool bAutoConnect = true;

	/** If true, a game session is requested as soon as the backend acknowledged hello */
	UPROPERTY(Config)
	bool bAutoCreateSession = true;

private:

	// automation tests drive the socket callbacks through a fake socket
	friend struct FRiftNetworkTestAccess;

	// socket callbacks, game thread
	void HandleSocketConnected();
	void HandleSocketConnectionError(const FString& Error);
	void HandleSocketClosed(int32 StatusCode, const FString& Reason, bool bWasClean);
	void HandleSocketMessage(const FString& Message);

	// one handler per server message type
	void HandleHelloAck(const FRiftEnvelope& Envelope);
	void HandlePong(const FRiftEnvelope& Envelope);
	void HandleSessionCreated(const FRiftEnvelope& Envelope);
	void HandleWorldEvent(const FRiftEnvelope& Envelope);
	void HandleError(const FRiftEnvelope& Envelope);

	/** Unbinds from the socket, closes it if still open and lets go of it */
	void CloseSocket();

	/** Forgets the session and any pending request for one */
	void ClearSession();

	/** Moves to Disconnected, drops the session and tells listeners why */
	void EnterDisconnected(const FString& Reason);

	TSharedPtr<IWebSocket> Socket;

	/** True from Connect until the socket reports an error or a close */
	bool bSocketActive = false;

	ERiftConnectionState State = ERiftConnectionState::Disconnected;

	FString SessionId;

	/** True from a sent create_session until session_created, its error or a disconnect */
	bool bSessionPending = false;

	/** message_id of the pending create_session, to recognise the error that answers it */
	FString SessionRequestId;

	/** Tests only: replaces the real WebSocket */
	TFunction<TSharedPtr<IWebSocket>()> SocketFactory;
};
