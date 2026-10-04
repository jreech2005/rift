// Rift WebSocket protocol V1 wire format. Contract: docs/PROTOCOL.md.

#include "RiftProtocol.h"

#include "Dom/JsonValue.h"
#include "Misc/DateTime.h"
#include "Misc/Guid.h"
#include "Policies/CondensedJsonPrintPolicy.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"

DEFINE_LOG_CATEGORY(LogRiftNet);

namespace RiftProtocol
{
	/** Writes a string field, or JSON null when the value is empty */
	static void SetStringOrNull(FJsonObject& Object, const TCHAR* FieldName, const FString& Value)
	{
		if (Value.IsEmpty())
		{
			Object.SetField(FieldName, MakeShared<FJsonValueNull>());
		}
		else
		{
			Object.SetStringField(FieldName, Value);
		}
	}

	FString NewId()
	{
		// FGuid::NewGuid is 128 random bits; stamp the RFC 4122 version 4 and variant bits so it is a real UUID
		FGuid Guid = FGuid::NewGuid();
		Guid.B = (Guid.B & 0xFFFF0FFF) | 0x00004000;
		Guid.C = (Guid.C & 0x3FFFFFFF) | 0x80000000;

		return Guid.ToString(EGuidFormats::DigitsWithHyphensLower);
	}

	FString NowTimestamp()
	{
		return FDateTime::UtcNow().ToIso8601();
	}

	FString ToJson(const TSharedPtr<FJsonObject>& Object)
	{
		FString Out;

		if (Object.IsValid())
		{
			const TSharedRef<TJsonWriter<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>> Writer =
				TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>::Create(&Out);

			FJsonSerializer::Serialize(Object, Writer);
		}

		return Out;
	}

	FString BuildEnvelope(const FString& MessageType, const FString& SessionId, const TSharedPtr<FJsonObject>& Payload, FString* OutMessageId)
	{
		const FString MessageId = NewId();

		// reply_to is server only and the backend rejects unknown fields: send exactly these six
		const TSharedPtr<FJsonObject> Envelope = MakeShared<FJsonObject>();
		Envelope->SetNumberField(TEXT("protocol_version"), Version);
		Envelope->SetStringField(TEXT("message_id"), MessageId);
		Envelope->SetStringField(TEXT("message_type"), MessageType);
		Envelope->SetStringField(TEXT("timestamp"), NowTimestamp());
		SetStringOrNull(*Envelope, TEXT("session_id"), SessionId);
		Envelope->SetObjectField(TEXT("payload"), Payload.IsValid() ? Payload : TSharedPtr<FJsonObject>(MakeShared<FJsonObject>()));

		if (OutMessageId)
		{
			*OutMessageId = MessageId;
		}

		return ToJson(Envelope);
	}

	TSharedRef<FJsonObject> MakeHello(const FString& Client, const FString& ClientVersion)
	{
		const TSharedRef<FJsonObject> Hello = MakeShared<FJsonObject>();
		Hello->SetStringField(TEXT("client"), Client);
		Hello->SetStringField(TEXT("client_version"), ClientVersion);
		return Hello;
	}

	TSharedRef<FJsonObject> MakePing(const FString& Nonce)
	{
		const TSharedRef<FJsonObject> Ping = MakeShared<FJsonObject>();

		if (!Nonce.IsEmpty())
		{
			Ping->SetStringField(TEXT("nonce"), Nonce);
		}

		return Ping;
	}

	TSharedRef<FJsonObject> MakePlayerAction(const FString& ActionId, const FString& SessionId, const FString& ActionType, const FString& Target, const FString& Content)
	{
		const TSharedRef<FJsonObject> Action = MakeShared<FJsonObject>();
		Action->SetNumberField(TEXT("protocol_version"), Version);
		Action->SetStringField(TEXT("action_id"), ActionId);
		Action->SetStringField(TEXT("session_id"), SessionId);
		Action->SetStringField(TEXT("actor_id"), PlayerActorId);
		Action->SetStringField(TEXT("action_type"), ActionType);
		SetStringOrNull(*Action, TEXT("target"), Target);
		SetStringOrNull(*Action, TEXT("content"), Content);
		Action->SetStringField(TEXT("timestamp"), NowTimestamp());
		return Action;
	}

	bool ParseEnvelope(const FString& Text, FRiftEnvelope& OutEnvelope, FString& OutError)
	{
		TSharedPtr<FJsonValue> Root;
		const TSharedRef<TJsonReader<TCHAR>> Reader = TJsonReaderFactory<TCHAR>::Create(Text);

		if (!FJsonSerializer::Deserialize(Reader, Root) || !Root.IsValid())
		{
			OutError = TEXT("invalid JSON");
			return false;
		}

		const TSharedPtr<FJsonObject>* RootObject = nullptr;

		if (!Root->TryGetObject(RootObject) || !RootObject || !RootObject->IsValid())
		{
			OutError = TEXT("message is not a JSON object");
			return false;
		}

		const FJsonObject& Object = **RootObject;

		int32 ProtocolVersion = 0;

		if (!Object.TryGetNumberField(TEXT("protocol_version"), ProtocolVersion))
		{
			OutError = TEXT("missing or non-integer protocol_version");
			return false;
		}

		if (ProtocolVersion != Version)
		{
			OutError = FString::Printf(TEXT("protocol_version %d is not supported; client speaks %d"), ProtocolVersion, Version);
			return false;
		}

		FRiftEnvelope Envelope;

		if (!Object.TryGetStringField(TEXT("message_type"), Envelope.MessageType) || Envelope.MessageType.IsEmpty())
		{
			OutError = TEXT("missing message_type");
			return false;
		}

		// the remaining fields are optional on receive; a null session_id stays empty
		Object.TryGetStringField(TEXT("message_id"), Envelope.MessageId);
		Object.TryGetStringField(TEXT("timestamp"), Envelope.Timestamp);
		Object.TryGetStringField(TEXT("session_id"), Envelope.SessionId);
		Object.TryGetStringField(TEXT("reply_to"), Envelope.ReplyTo);

		const TSharedPtr<FJsonObject>* Payload = nullptr;

		if (Object.TryGetObjectField(TEXT("payload"), Payload) && Payload && Payload->IsValid())
		{
			Envelope.Payload = *Payload;
		}
		else
		{
			Envelope.Payload = MakeShared<FJsonObject>();
		}

		OutEnvelope = MoveTemp(Envelope);
		return true;
	}
}
