// Turns validated backend world events into visible game behavior.

#pragma once

#include "CoreMinimal.h"
#include "RiftNetworkSubsystem.h"
#include "RiftWorldEventTypes.h"
#include "Subsystems/WorldSubsystem.h"
#include "RiftWorldPresentationSubsystem.generated.h"

class ARiftNPC;
class UAudioComponent;
class URiftEntityComponent;

DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FRiftHudChangedDelegate, const FRiftHudState&, HudState);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FRiftObjectiveDelegate, const FRiftObjectiveState&, Objective);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FRiftMissionDelegate, const FString&, MissionId, const FString&, Status, const FString&, Title);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FRiftWorldFlagDelegate, const FString&, Flag, bool, bValue);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FRiftDialogueDelegate, const FString&, SpeakerId, const FString&, Line);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FRiftTriggeredEventDelegate, const FString&, EventName, const FString&, Description);

/**
 *  The presentation side of the backend connection. Listens to URiftNetworkSubsystem::OnWorldEvent and
 *  shows each event: HUD state, NPC movement and facing, voiced dialogue, per entity notifications.
 *  It only presents. Authoritative state stays in the backend and nothing here is sent back as a result of an event.
 *  One per game world. Actors join through URiftEntityComponent.
 */
UCLASS(Config=Game)
class RIFT_API URiftWorldPresentationSubsystem : public UTickableWorldSubsystem
{
	GENERATED_BODY()

public:

	virtual void OnWorldBeginPlay(UWorld& InWorld) override;
	virtual void Deinitialize() override;

	virtual void Tick(float DeltaTime) override;
	virtual TStatId GetStatId() const override;

	/** Called by URiftEntityComponent when its actor enters play */
	void RegisterEntity(URiftEntityComponent* Entity);

	/** Called by URiftEntityComponent when its actor leaves play */
	void UnregisterEntity(URiftEntityComponent* Entity);

	/** Returns the entity with this Rift id, null when the level has none */
	UFUNCTION(BlueprintPure, Category="Rift|Presentation")
	URiftEntityComponent* FindEntity(const FString& RiftId) const;

	/** Returns the display name of an entity, or the id itself when the level has no such entity */
	UFUNCTION(BlueprintPure, Category="Rift|Presentation")
	FString GetDisplayName(const FString& RiftId) const;

	/** What the HUD should show right now */
	UFUNCTION(BlueprintPure, Category="Rift|Presentation")
	FRiftHudState GetHudState() const { return HudModel.State; }

	/** Returns the last value the backend reported for a world flag, false when it never did */
	UFUNCTION(BlueprintPure, Category="Rift|Presentation")
	bool GetWorldFlag(const FString& Flag) const;

	/** Returns a string field of an event's payload, empty when missing. For Blueprint handlers of raw events */
	UFUNCTION(BlueprintPure, Category="Rift|Presentation")
	static FString GetPayloadString(const FRiftWorldEvent& Event, const FString& Field);

	/**
	 *  Presents an event made on this machine as if the backend had sent it. Nothing is sent to the backend
	 *  and no authoritative state changes. For testing the level without a server.
	 */
	UFUNCTION(BlueprintCallable, Category="Rift|Presentation")
	void PresentLocalEvent(const FString& EventType, const FString& Target, const FString& PayloadJson);

	/** The HUD state changed */
	UPROPERTY(BlueprintAssignable, Category="Rift|Presentation")
	FRiftHudChangedDelegate OnHudChanged;

	UPROPERTY(BlueprintAssignable, Category="Rift|Presentation")
	FRiftObjectiveDelegate OnObjectiveUpdated;

	UPROPERTY(BlueprintAssignable, Category="Rift|Presentation")
	FRiftMissionDelegate OnMissionUpdated;

	UPROPERTY(BlueprintAssignable, Category="Rift|Presentation")
	FRiftWorldFlagDelegate OnWorldFlagChanged;

	/** An NPC opened a dialogue or told the player something */
	UPROPERTY(BlueprintAssignable, Category="Rift|Presentation")
	FRiftDialogueDelegate OnDialogueLine;

	/** world_event_triggered: something happened in the world, for example confrontation_begins */
	UPROPERTY(BlueprintAssignable, Category="Rift|Presentation")
	FRiftTriggeredEventDelegate OnWorldEventTriggered;

	/** Every world event, known type or not, after the built in presentation ran */
	UPROPERTY(BlueprintAssignable, Category="Rift|Presentation")
	FRiftWorldEventDelegate OnAnyWorldEvent;

protected:

	virtual bool DoesSupportWorldType(const EWorldType::Type WorldType) const override;

	/**
	 *  If true a game session is requested as soon as the backend is connected and the level holds
	 *  at least one Rift entity. Levels without Rift entities never create a session on their own.
	 */
	UPROPERTY(Config)
	bool bAutoCreateSession = true;

	/** If true a dialogue_started event with an audio_url is fetched from the backend and played. Subtitles show either way */
	UPROPERTY(Config)
	bool bPlayVoice = true;

	/** Volume of voiced NPC lines */
	UPROPERTY(Config)
	float VoiceVolume = 1.0f;

private:

	UFUNCTION()
	void HandleWorldEvent(const FRiftWorldEvent& Event);

	UFUNCTION()
	void HandleReady();

	UFUNCTION()
	void HandleDisconnected(const FString& Reason);

	UFUNCTION()
	void HandleSessionCreated(const FString& SessionId);

	UFUNCTION()
	void HandleBackendError(const FString& Code, const FString& Message);

	URiftNetworkSubsystem* GetNetwork() const;

	/** Asks for a session if this level wants one and none exists or is on its way */
	void CreateSessionIfNeeded();

	/** Moves NPCs and tells entities about the event */
	void RouteToWorld(const FRiftParsedWorldEvent& Parsed);

	ARiftNPC* FindNpc(const FString& NpcId) const;

	/** Sends the NPC to the marker for MarkerId. Does nothing when either is missing from the level */
	void MoveNpcToMarker(const FString& NpcId, const FString& MarkerId);

	/** Fetches a voiced line from the backend without blocking and plays it when it arrives. Failures only log */
	void RequestVoice(const FString& AudioUrl);

	/** Plays a fetched clip, unless a newer line was requested in the meantime */
	void PlayVoiceClip(int32 RequestId, const TArray<uint8>& Bytes);

	void StopVoice();

	double Now() const;

	TMap<FString, TWeakObjectPtr<URiftEntityComponent>> Entities;

	FRiftHudModel HudModel;

	/** The line being spoken, null when silent */
	UPROPERTY(Transient)
	TObjectPtr<UAudioComponent> VoiceComponent;

	/** Counts voice requests, a clip that arrives after a newer request is dropped */
	int32 VoiceRequestId = 0;

	double VoiceEnds = 0.0;
};
