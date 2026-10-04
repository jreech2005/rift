// A place in the level that backend events can send an NPC to.

#pragma once

#include "CoreMinimal.h"
#include "Engine/TargetPoint.h"
#include "RiftLocationMarker.generated.h"

/**
 *  Names a spot in the level. MarkerId is either a backend location id (npc_moved, npc_activated)
 *  or the name of a triggered world event (world_event_triggered), for example confrontation_begins.
 *  Place it on the NavMesh. The backend decides where an NPC goes, navigation decides how.
 */
UCLASS()
class RIFT_API ARiftLocationMarker : public ATargetPoint
{
	GENERATED_BODY()

public:

	/** Location id or world event name this spot stands for */
	UPROPERTY(EditAnywhere, BlueprintReadOnly, Category="Rift")
	FString MarkerId;

	/** If set, the spot is reserved for this NPC. Leave empty for a spot any NPC may use */
	UPROPERTY(EditAnywhere, BlueprintReadOnly, Category="Rift")
	FString NpcId;

	/**
	 *  Returns the marker for MarkerId that fits NpcId best: one reserved for that NPC, else an unreserved one.
	 *  Null when the level has none.
	 */
	static ARiftLocationMarker* Find(const UWorld* World, const FString& InMarkerId, const FString& InNpcId);
};
