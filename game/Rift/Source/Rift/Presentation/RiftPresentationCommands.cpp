// Console commands for driving the presentation layer by hand.

#include "Engine/World.h"
#include "HAL/IConsoleManager.h"
#include "RiftEntityComponent.h"
#include "RiftProtocol.h"
#include "RiftWorldEventTypes.h"
#include "RiftWorldPresentationSubsystem.h"

namespace
{
	/**
	 *  Builds a payload from console words. Either one JSON object, or key=value pairs where a value
	 *  runs until the next key: status=failed title=Hide the phone. true and false become booleans,
	 *  npc_ids takes a comma separated list.
	 */
	FString MakePayloadJson(const TArray<FString>& Words)
	{
		const FString Joined = FString::Join(Words, TEXT(" "));

		if (Joined.StartsWith(TEXT("{")))
		{
			return Joined;
		}

		TArray<TPair<FString, FString>> Fields;

		for (const FString& Word : Words)
		{
			FString Key;
			FString Value;

			if (Word.Split(TEXT("="), &Key, &Value) && !Key.IsEmpty())
			{
				Fields.Emplace(Key, Value);
			}
			else if (Fields.Num() > 0)
			{
				Fields.Last().Value += TEXT(" ") + Word;
			}
		}

		const TSharedPtr<FJsonObject> Payload = MakeShared<FJsonObject>();

		for (const TPair<FString, FString>& Field : Fields)
		{
			if (Field.Key == TEXT("npc_ids"))
			{
				TArray<FString> Ids;
				Field.Value.ParseIntoArray(Ids, TEXT(","));

				TArray<TSharedPtr<FJsonValue>> Values;

				for (const FString& Id : Ids)
				{
					Values.Add(MakeShared<FJsonValueString>(Id.TrimStartAndEnd()));
				}

				Payload->SetArrayField(Field.Key, Values);
			}
			else if (Field.Value == TEXT("true") || Field.Value == TEXT("false"))
			{
				Payload->SetBoolField(Field.Key, Field.Value == TEXT("true"));
			}
			else
			{
				Payload->SetStringField(Field.Key, Field.Value);
			}
		}

		return RiftProtocol::ToJson(Payload);
	}

	URiftWorldPresentationSubsystem* GetPresentation(UWorld* World, const TCHAR* Command)
	{
		URiftWorldPresentationSubsystem* Presentation = World ? World->GetSubsystem<URiftWorldPresentationSubsystem>() : nullptr;

		if (!Presentation)
		{
			UE_LOG(LogRiftPresentation, Warning, TEXT("%s needs a running game: start Play In Editor or launch with -game"), Command);
		}

		return Presentation;
	}

	void RunFakeEvent(const TArray<FString>& Args, UWorld* World)
	{
		if (Args.Num() < 2)
		{
			UE_LOG(LogRiftPresentation, Warning, TEXT("Usage: Rift.FakeEvent <event_type> <target or -> [key=value ... | {json}]"));
			return;
		}

		if (URiftWorldPresentationSubsystem* Presentation = GetPresentation(World, TEXT("Rift.FakeEvent")))
		{
			TArray<FString> PayloadWords = Args;
			PayloadWords.RemoveAt(0, 2);

			const FString Target = Args[1] == TEXT("-") ? FString() : Args[1];

			Presentation->PresentLocalEvent(Args[0], Target, MakePayloadJson(PayloadWords));
		}
	}

	void RunInteract(const TArray<FString>& Args, UWorld* World)
	{
		if (Args.Num() < 1)
		{
			UE_LOG(LogRiftPresentation, Warning, TEXT("Usage: Rift.Interact <rift_id>"));
			return;
		}

		if (const URiftWorldPresentationSubsystem* Presentation = GetPresentation(World, TEXT("Rift.Interact")))
		{
			URiftEntityComponent* Entity = Presentation->FindEntity(Args[0]);

			if (!Entity)
			{
				UE_LOG(LogRiftPresentation, Warning, TEXT("Rift.Interact: the level has no entity with Rift id '%s'"), *Args[0]);
			}
			else if (!Entity->Interact())
			{
				UE_LOG(LogRiftPresentation, Warning, TEXT("Rift.Interact failed to send"));
			}
		}
	}

	FAutoConsoleCommandWithWorldAndArgs FakeEventCommand(
		TEXT("Rift.FakeEvent"),
		TEXT("Presents a world event locally, nothing is sent to the backend: Rift.FakeEvent <event_type> <target or -> [key=value ... | {json}]"),
		FConsoleCommandWithWorldAndArgsDelegate::CreateStatic(&RunFakeEvent));

	FAutoConsoleCommandWithWorldAndArgs InteractCommand(
		TEXT("Rift.Interact"),
		TEXT("Interacts with a Rift entity in the level as if the player pressed E on it: Rift.Interact <rift_id>"),
		FConsoleCommandWithWorldAndArgsDelegate::CreateStatic(&RunInteract));
}
