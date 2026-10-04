// Gives an actor a Rift id so backend events can find it and the player can act on it.

#pragma once

#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "RiftNetworkSubsystem.h"
#include "RiftEntityComponent.generated.h"

/**
 *  Marks the owning actor as a Rift entity: an NPC, a prop, a door.
 *  RiftId is the id the backend uses for it, for example as the target of a player_action or a world_event.
 *  Registers with URiftWorldPresentationSubsystem while the actor is in play.
 */
UCLASS(ClassGroup=(Rift), meta=(BlueprintSpawnableComponent, DisplayName="Rift Entity"))
class RIFT_API URiftEntityComponent : public UActorComponent
{
	GENERATED_BODY()

public:

	URiftEntityComponent();

	/** Backend id of this entity. Letters, digits and _ . : - only, at most 64 characters */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category="Rift")
	FString RiftId;

	/** Name shown to the player. The id is shown when this is empty */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category="Rift")
	FString DisplayName;

	/** If false the player cannot interact with this entity, events still reach it */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category="Rift|Interaction")
	bool bInteractable = true;

	/** action_type sent by Interact: interact, inspect, move or speak */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category="Rift|Interaction")
	FString InteractActionType = TEXT("interact");

	/** Content sent by Interact. Required for speak, must be empty for the other action types */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category="Rift|Interaction", meta=(MultiLine=true))
	FString InteractContent;

	/** Sends this entity's configured player_action to the backend. Returns false if it could not be sent */
	UFUNCTION(BlueprintCallable, Category="Rift|Interaction")
	bool Interact();

	/** Sends any player_action with this entity as the target */
	UFUNCTION(BlueprintCallable, Category="Rift|Interaction")
	bool SendPlayerAction(const FString& ActionType, const FString& Content);

	/** Returns DisplayName, or the id when no name is set */
	UFUNCTION(BlueprintPure, Category="Rift")
	FString GetDisplayNameOrId() const;

	/** A world event that names this entity arrived. Present it, do not treat it as a request */
	UPROPERTY(BlueprintAssignable, Category="Rift")
	FRiftWorldEventDelegate OnRiftEvent;

protected:

	virtual void BeginPlay() override;
	virtual void EndPlay(const EEndPlayReason::Type EndPlayReason) override;
};
