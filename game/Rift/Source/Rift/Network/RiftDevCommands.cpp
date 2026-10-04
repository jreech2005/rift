#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "HAL/IConsoleManager.h"
#include "RiftNetworkSubsystem.h"

namespace
{
void RunRiftSpeak(const TArray<FString>& Args, UWorld* World)
{
    if (Args.Num() < 2)
    {
        UE_LOG(
            LogTemp,
            Warning,
            TEXT("Usage: Rift.Speak <npc_id> <dialogue...>")
        );
        return;
    }

    UGameInstance* GameInstance = World ? World->GetGameInstance() : nullptr;
    URiftNetworkSubsystem* Network =
        GameInstance ? GameInstance->GetSubsystem<URiftNetworkSubsystem>() : nullptr;

    if (!Network)
    {
        UE_LOG(
            LogTemp,
            Warning,
            TEXT("Rift.Speak needs a running game: start Play In Editor first")
        );
        return;
    }

    FString Content;
    for (int32 Index = 1; Index < Args.Num(); ++Index)
    {
        if (!Content.IsEmpty())
        {
            Content += TEXT(" ");
        }
        Content += Args[Index];
    }

    const FString& Target = Args[0];

    UE_LOG(
        LogTemp,
        Display,
        TEXT("Rift.Speak -> %s: %s"),
        *Target,
        *Content
    );

    if (!Network->SendPlayerAction(TEXT("speak"), Target, Content))
    {
        UE_LOG(
            LogTemp,
            Warning,
            TEXT("Rift.Speak failed to send")
        );
    }
}

FAutoConsoleCommandWithWorldAndArgs RiftSpeakCommand(
    TEXT("Rift.Speak"),
    TEXT("Send dialogue to an NPC: Rift.Speak <npc_id> <dialogue...>"),
    FConsoleCommandWithWorldAndArgsDelegate::CreateStatic(&RunRiftSpeak)
);
}
