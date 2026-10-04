// Typed view of backend world events and the HUD state they produce. Event names: docs/PROTOCOL.md.

#pragma once

#include "CoreMinimal.h"
#include "Dom/JsonObject.h"
#include "RiftNetworkSubsystem.h"
#include "RiftWorldEventTypes.generated.h"

/** Log category for everything that presents backend events in the world */
DECLARE_LOG_CATEGORY_EXTERN(LogRiftPresentation, Log, All);

/** The world_event types of protocol V1. V1 may add types, those read as Unknown */
UENUM(BlueprintType)
enum class ERiftWorldEventType : uint8
{
	Unknown,
	InteractionAcknowledged,
	InspectionAcknowledged,
	LocationChanged,
	SpeechAcknowledged,
	ObjectiveUpdated,
	MissionUpdated,
	NpcActivated,
	NpcMoved,
	NpcDispositionChanged,
	InformationRevealed,
	WorldFlagChanged,
	WorldEventTriggered,
	DialogueStarted
};

/** One world event with its type resolved and its payload decoded */
struct FRiftParsedWorldEvent
{
	FRiftWorldEvent Event;

	ERiftWorldEventType Type = ERiftWorldEventType::Unknown;

	/** Never null. Empty when the event had no payload or the payload was not a JSON object */
	TSharedRef<FJsonObject> Payload = MakeShared<FJsonObject>();

	/** Returns a payload string field, empty when missing, null or not a string */
	FString GetString(const TCHAR* Field) const;

	/** Returns a payload array of strings, empty when missing */
	TArray<FString> GetStringArray(const TCHAR* Field) const;
};

namespace RiftEvents
{
	/** objective_updated and mission_updated status values */
	namespace Status
	{
		inline const TCHAR* const Active = TEXT("active");
		inline const TCHAR* const Completed = TEXT("completed");
		inline const TCHAR* const Failed = TEXT("failed");
		inline const TCHAR* const Invalidated = TEXT("invalidated");
	}

	/** actor id of the player in event payloads */
	inline const TCHAR* const PlayerId = TEXT("player");

	/** Maps an event_type string to its enum value. Compared exactly, unknown names give Unknown */
	ERiftWorldEventType Classify(const FString& EventType);

	/** Resolves the type and decodes PayloadJson. Never fails: a bad payload becomes an empty object */
	FRiftParsedWorldEvent Parse(const FRiftWorldEvent& Event);
}

/** An objective as the backend last described it */
USTRUCT(BlueprintType)
struct FRiftObjectiveState
{
	GENERATED_BODY()

	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FString ObjectiveId;

	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FString MissionId;

	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FString Title;

	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FString Description;

	/** active, completed, failed or invalidated */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FString Status;
};

/** Everything the player facing HUD shows. Empty strings mean nothing to show */
USTRUCT(BlueprintType)
struct FRiftHudState
{
	GENERATED_BODY()

	/** False until the first objective_updated arrives */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	bool bHasObjective = false;

	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FRiftObjectiveState CurrentObjective;

	/** True while the shown objective is failed or invalidated and nothing replaced it yet */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	bool bObjectiveFailed = false;

	/** NEW OBJECTIVE, OBJECTIVE FAILED, MISSION FAILED and so on. Cleared when it expires */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FString Banner;

	/** Rift id of who is speaking, may be empty */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FString SubtitleSpeakerId;

	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FString Subtitle;

	/** Description of the last triggered world event */
	UPROPERTY(BlueprintReadOnly, Category="Rift|Presentation")
	FString Notice;
};

/**
 *  Turns world events into HUD state. Plain data and logic, no world access, so it can be tested alone.
 *  Times are seconds on any monotonic clock, the caller passes the same clock to Apply and Advance.
 */
struct FRiftHudModel
{
	FRiftHudState State;

	TMap<FString, bool> WorldFlags;

	/** Applies one event. Unknown types and missing fields change nothing. Returns true if State changed */
	bool Apply(const FRiftParsedWorldEvent& Parsed, double Now);

	/** Expires the banner, subtitle and notice and shows the next queued banner. Returns true if State changed */
	bool Advance(double Now);

	static constexpr double BannerSeconds = 3.5;
	static constexpr double NoticeSeconds = 8.0;

private:

	void ApplyObjective(const FRiftParsedWorldEvent& Parsed);
	void ApplyMission(const FRiftParsedWorldEvent& Parsed);
	void ShowSubtitle(const FString& SpeakerId, const FString& Line, double Now);

	/** Objectives currently active, oldest first */
	TArray<FRiftObjectiveState> ActiveObjectives;

	/** Banners waiting for the current one to expire */
	TArray<FString> PendingBanners;

	/** session:sequence of every applied event, a resent event is applied once */
	TSet<FString> SeenEvents;

	double BannerExpires = 0.0;
	double SubtitleExpires = 0.0;
	double NoticeExpires = 0.0;
};
