// Gives an actor a Rift id so backend events can find it and the player can act on it.

#include "RiftEntityComponent.h"

#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "RiftWorldEventTypes.h"
#include "RiftWorldPresentationSubsystem.h"

URiftEntityComponent::URiftEntityComponent()
{
	PrimaryComponentTick.bCanEverTick = false;
}

void URiftEntityComponent::BeginPlay()
{
	Super::BeginPlay();

	if (URiftWorldPresentationSubsystem* Presentation = GetWorld()->GetSubsystem<URiftWorldPresentationSubsystem>())
	{
		Presentation->RegisterEntity(this);
	}
}

void URiftEntityComponent::EndPlay(const EEndPlayReason::Type EndPlayReason)
{
	if (URiftWorldPresentationSubsystem* Presentation = GetWorld()->GetSubsystem<URiftWorldPresentationSubsystem>())
	{
		Presentation->UnregisterEntity(this);
	}

	Super::EndPlay(EndPlayReason);
}

bool URiftEntityComponent::Interact()
{
	if (!bInteractable)
	{
		return false;
	}

	return SendPlayerAction(InteractActionType, InteractContent);
}

bool URiftEntityComponent::SendPlayerAction(const FString& ActionType, const FString& Content)
{
	if (RiftId.IsEmpty())
	{
		UE_LOG(LogRiftPresentation, Warning, TEXT("%s has a Rift Entity component without a Rift id"), *GetNameSafe(GetOwner()));
		return false;
	}

	const UGameInstance* GameInstance = GetWorld()->GetGameInstance();
	URiftNetworkSubsystem* Network = GameInstance ? GameInstance->GetSubsystem<URiftNetworkSubsystem>() : nullptr;

	if (!Network)
	{
		return false;
	}

	UE_LOG(LogRiftPresentation, Log, TEXT("player_action %s -> %s %s"), *ActionType, *RiftId, *Content);

	return Network->SendPlayerAction(ActionType, RiftId, Content);
}

FString URiftEntityComponent::GetDisplayNameOrId() const
{
	return DisplayName.IsEmpty() ? RiftId : DisplayName;
}
