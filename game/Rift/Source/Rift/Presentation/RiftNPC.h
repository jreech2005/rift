// A character the backend can address by Rift id: it can face the player and walk to a marker.

#pragma once

#include "CoreMinimal.h"
#include "GameFramework/Character.h"
#include "RiftNPC.generated.h"

class ARiftLocationMarker;
class URiftEntityComponent;
class UTextRenderComponent;

/**
 *  Generic Rift NPC. Its Rift Entity component carries the npc id and display name.
 *  Possessed by a plain AAIController and moved with normal Unreal navigation, so the level needs a NavMesh.
 *  Set the mesh and animation in a Blueprint child or on the placed instance.
 */
UCLASS()
class RIFT_API ARiftNPC : public ACharacter
{
	GENERATED_BODY()

public:

	ARiftNPC();

	virtual void Tick(float DeltaSeconds) override;

	/** Turns toward the player and keeps facing them until the next move */
	UFUNCTION(BlueprintCallable, Category="Rift|NPC")
	void FacePlayer();

	/** Walks to the marker along the NavMesh. Returns false if no path request could be made */
	UFUNCTION(BlueprintCallable, Category="Rift|NPC")
	bool MoveToMarker(ARiftLocationMarker* Marker);

	/** Called by the presentation layer when the backend starts a dialogue with this NPC */
	void PresentDialogue(const FString& Line);

	UFUNCTION(BlueprintPure, Category="Rift|NPC")
	URiftEntityComponent* GetRiftEntity() const { return RiftEntity; }

protected:

	virtual void BeginPlay() override;

	/** The backend started a dialogue with this NPC. For voice, animation or other polish */
	UFUNCTION(BlueprintImplementableEvent, Category="Rift|NPC", meta=(DisplayName="Rift Dialogue Started"))
	void BP_OnRiftDialogue(const FString& Line);

	/** This NPC started walking to a marker */
	UFUNCTION(BlueprintImplementableEvent, Category="Rift|NPC", meta=(DisplayName="Rift Move Started"))
	void BP_OnRiftMoveStarted(ARiftLocationMarker* Marker);

	UPROPERTY(VisibleAnywhere, BlueprintReadOnly, Category="Rift")
	TObjectPtr<URiftEntityComponent> RiftEntity;

	/** Floating name above the head, always turned to the player */
	UPROPERTY(VisibleAnywhere, BlueprintReadOnly, Category="Rift")
	TObjectPtr<UTextRenderComponent> NameTag;

	/** How close to a marker counts as arrived, in cm */
	UPROPERTY(EditAnywhere, Category="Rift|NPC")
	float MarkerAcceptanceRadius = 60.0f;

	/** Turn speed while facing the player, in degrees per second */
	UPROPERTY(EditAnywhere, Category="Rift|NPC")
	float FaceTurnSpeed = 360.0f;

	/** If true the NPC turns to the player once it reaches a marker */
	UPROPERTY(EditAnywhere, Category="Rift|NPC")
	bool bFacePlayerOnArrival = true;

private:

	bool bFacingPlayer = false;
	bool bWalkingToMarker = false;
};
