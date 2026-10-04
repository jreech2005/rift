// A place in the level that backend events can send an NPC to.

#include "RiftLocationMarker.h"

#include "EngineUtils.h"

ARiftLocationMarker* ARiftLocationMarker::Find(const UWorld* World, const FString& InMarkerId, const FString& InNpcId)
{
	if (!World || InMarkerId.IsEmpty())
	{
		return nullptr;
	}

	ARiftLocationMarker* Unreserved = nullptr;

	for (TActorIterator<ARiftLocationMarker> It(World); It; ++It)
	{
		ARiftLocationMarker* Marker = *It;

		if (!Marker->MarkerId.Equals(InMarkerId, ESearchCase::CaseSensitive))
		{
			continue;
		}

		if (Marker->NpcId.IsEmpty())
		{
			if (!Unreserved)
			{
				Unreserved = Marker;
			}
		}
		else if (Marker->NpcId.Equals(InNpcId, ESearchCase::CaseSensitive))
		{
			return Marker;
		}
	}

	return Unreserved;
}
