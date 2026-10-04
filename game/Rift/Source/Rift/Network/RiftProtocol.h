// Rift WebSocket protocol V1 wire format. Contract: docs/PROTOCOL.md.

#pragma once

#include "CoreMinimal.h"
#include "Dom/JsonObject.h"

/** Log category for everything that talks to the Rift backend */
DECLARE_LOG_CATEGORY_EXTERN(LogRiftNet, Log, All);

/** One decoded protocol V1 envelope */
struct FRiftEnvelope
{
	FString MessageId;
	FString MessageType;
	FString Timestamp;

	/** Empty when the envelope's session_id is null */
	FString SessionId;

	/** Server only: the message_id this message answers. Empty when absent */
	FString ReplyTo;

	/** Never null after a successful parse */
	TSharedPtr<FJsonObject> Payload;
};

/**
 *  Builds and parses protocol V1 messages.
 *  Envelope and client payload field names live in RiftProtocol.cpp so the outgoing contract is in one place.
 */
namespace RiftProtocol
{
	/** The only protocol version this client speaks */
	inline constexpr int32 Version = 1;

	/** actor_id sent with player actions */
	inline const TCHAR* const PlayerActorId = TEXT("player");

	namespace MessageType
	{
		// client -> server
		inline const TCHAR* const Hello = TEXT("hello");
		inline const TCHAR* const Ping = TEXT("ping");
		inline const TCHAR* const CreateSession = TEXT("create_session");
		inline const TCHAR* const PlayerAction = TEXT("player_action");

		// server -> client
		inline const TCHAR* const HelloAck = TEXT("hello_ack");
		inline const TCHAR* const Pong = TEXT("pong");
		inline const TCHAR* const SessionCreated = TEXT("session_created");
		inline const TCHAR* const WorldEvent = TEXT("world_event");
		inline const TCHAR* const Error = TEXT("error");
	}

	/** Returns a new lowercase hyphenated UUID, used for message_id and action_id */
	FString NewId();

	/** Returns the current UTC time as an RFC 3339 date-time */
	FString NowTimestamp();

	/** Serializes a JSON object without whitespace */
	FString ToJson(const TSharedPtr<FJsonObject>& Object);

	/**
	 *  Serializes a client envelope. An empty SessionId is sent as null, a null Payload as {}.
	 *  @param OutMessageId	optionally receives the generated message_id
	 */
	FString BuildEnvelope(const FString& MessageType, const FString& SessionId, const TSharedPtr<FJsonObject>& Payload, FString* OutMessageId = nullptr);

	/** Builds a hello payload */
	TSharedRef<FJsonObject> MakeHello(const FString& Client, const FString& ClientVersion);

	/** Builds a ping payload. An empty Nonce is omitted */
	TSharedRef<FJsonObject> MakePing(const FString& Nonce);

	/** Builds a player_action payload. Empty Target or Content are sent as null */
	TSharedRef<FJsonObject> MakePlayerAction(const FString& ActionId, const FString& SessionId, const FString& ActionType, const FString& Target, const FString& Content);

	/**
	 *  Decodes one received text frame.
	 *  Fails on invalid JSON, non-object messages, a protocol_version other than 1 and a missing message_type.
	 *  Unknown fields and unknown message types are accepted, V1 may add them.
	 */
	bool ParseEnvelope(const FString& Text, FRiftEnvelope& OutEnvelope, FString& OutError);
}
