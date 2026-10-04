// Typed view of backend world events and the HUD state they produce. Event names: docs/PROTOCOL.md.

#include "RiftWorldEventTypes.h"

#include "Dom/JsonValue.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"

DEFINE_LOG_CATEGORY(LogRiftPresentation);

namespace
{
	struct FEventTypeName
	{
		const TCHAR* Name;
		ERiftWorldEventType Type;
	};

	const FEventTypeName EventTypeNames[] = {
		{ TEXT("interaction_acknowledged"), ERiftWorldEventType::InteractionAcknowledged },
		{ TEXT("inspection_acknowledged"), ERiftWorldEventType::InspectionAcknowledged },
		{ TEXT("location_changed"), ERiftWorldEventType::LocationChanged },
		{ TEXT("speech_acknowledged"), ERiftWorldEventType::SpeechAcknowledged },
		{ TEXT("objective_updated"), ERiftWorldEventType::ObjectiveUpdated },
		{ TEXT("mission_updated"), ERiftWorldEventType::MissionUpdated },
		{ TEXT("npc_activated"), ERiftWorldEventType::NpcActivated },
		{ TEXT("npc_moved"), ERiftWorldEventType::NpcMoved },
		{ TEXT("npc_disposition_changed"), ERiftWorldEventType::NpcDispositionChanged },
		{ TEXT("information_revealed"), ERiftWorldEventType::InformationRevealed },
		{ TEXT("world_flag_changed"), ERiftWorldEventType::WorldFlagChanged },
		{ TEXT("world_event_triggered"), ERiftWorldEventType::WorldEventTriggered },
		{ TEXT("dialogue_started"), ERiftWorldEventType::DialogueStarted },
	};

	bool IsStatus(const FString& Status, const TCHAR* Expected)
	{
		return Status.Equals(Expected, ESearchCase::CaseSensitive);
	}

	bool IsFailure(const FString& Status)
	{
		return IsStatus(Status, RiftEvents::Status::Failed) || IsStatus(Status, RiftEvents::Status::Invalidated);
	}
}

FString FRiftParsedWorldEvent::GetString(const TCHAR* Field) const
{
	FString Value;
	Payload->TryGetStringField(Field, Value);
	return Value;
}

TArray<FString> FRiftParsedWorldEvent::GetStringArray(const TCHAR* Field) const
{
	TArray<FString> Values;
	Payload->TryGetStringArrayField(Field, Values);
	return Values;
}

namespace RiftEvents
{
	ERiftWorldEventType Classify(const FString& EventType)
	{
		for (const FEventTypeName& Entry : EventTypeNames)
		{
			if (EventType.Equals(Entry.Name, ESearchCase::CaseSensitive))
			{
				return Entry.Type;
			}
		}

		return ERiftWorldEventType::Unknown;
	}

	FRiftParsedWorldEvent Parse(const FRiftWorldEvent& Event)
	{
		FRiftParsedWorldEvent Parsed;
		Parsed.Event = Event;
		Parsed.Type = Classify(Event.EventType);

		if (!Event.PayloadJson.IsEmpty())
		{
			TSharedPtr<FJsonObject> Object;
			const TSharedRef<TJsonReader<TCHAR>> Reader = TJsonReaderFactory<TCHAR>::Create(Event.PayloadJson);

			if (FJsonSerializer::Deserialize(Reader, Object) && Object.IsValid())
			{
				Parsed.Payload = Object.ToSharedRef();
			}
		}

		return Parsed;
	}
}

bool FRiftHudModel::Apply(const FRiftParsedWorldEvent& Parsed, double Now)
{
	// locally made events have no sequence and are never treated as repeats
	if (Parsed.Event.Sequence > 0)
	{
		bool bAlreadySeen = false;
		SeenEvents.Add(FString::Printf(TEXT("%s:%lld"), *Parsed.Event.SessionId, Parsed.Event.Sequence), &bAlreadySeen);

		if (bAlreadySeen)
		{
			return false;
		}
	}

	switch (Parsed.Type)
	{
	case ERiftWorldEventType::ObjectiveUpdated:
		ApplyObjective(Parsed);
		break;

	case ERiftWorldEventType::MissionUpdated:
		ApplyMission(Parsed);
		break;

	case ERiftWorldEventType::WorldFlagChanged:
	{
		const FString Flag = Parsed.GetString(TEXT("flag"));

		if (Flag.IsEmpty())
		{
			return false;
		}

		bool bValue = true;
		Parsed.Payload->TryGetBoolField(TEXT("value"), bValue);
		WorldFlags.Add(Flag, bValue);

		// flags are state for gameplay to read, the HUD shows nothing for them
		return false;
	}

	case ERiftWorldEventType::DialogueStarted:
	{
		const FString Speaker = Parsed.GetString(TEXT("npc_id"));
		ShowSubtitle(Speaker.IsEmpty() ? Parsed.Event.Target : Speaker, Parsed.GetString(TEXT("opening_line")), Now);
		break;
	}

	case ERiftWorldEventType::InformationRevealed:
		// the text is only sent when the player is the one who learns it
		if (Parsed.GetString(TEXT("recipient_id")).Equals(RiftEvents::PlayerId, ESearchCase::CaseSensitive))
		{
			ShowSubtitle(Parsed.GetString(TEXT("source_npc_id")), Parsed.GetString(TEXT("text")), Now);
		}
		break;

	case ERiftWorldEventType::WorldEventTriggered:
	{
		const FString Description = Parsed.GetString(TEXT("description"));

		if (!Description.IsEmpty())
		{
			State.Notice = Description;
			NoticeExpires = Now + NoticeSeconds;
		}
		break;
	}

	default:
		return false;
	}

	Advance(Now);
	return true;
}

bool FRiftHudModel::Advance(double Now)
{
	bool bChanged = false;

	if (Now >= BannerExpires && (!State.Banner.IsEmpty() || PendingBanners.Num() > 0))
	{
		if (PendingBanners.Num() > 0)
		{
			State.Banner = PendingBanners[0];
			PendingBanners.RemoveAt(0);
			BannerExpires = Now + BannerSeconds;
		}
		else
		{
			State.Banner.Reset();
		}

		bChanged = true;
	}

	if (!State.Subtitle.IsEmpty() && Now >= SubtitleExpires)
	{
		State.Subtitle.Reset();
		State.SubtitleSpeakerId.Reset();
		bChanged = true;
	}

	if (!State.Notice.IsEmpty() && Now >= NoticeExpires)
	{
		State.Notice.Reset();
		bChanged = true;
	}

	return bChanged;
}

void FRiftHudModel::ApplyObjective(const FRiftParsedWorldEvent& Parsed)
{
	FRiftObjectiveState Objective;
	Objective.ObjectiveId = Parsed.GetString(TEXT("objective_id"));
	Objective.MissionId = Parsed.GetString(TEXT("mission_id"));
	Objective.Title = Parsed.GetString(TEXT("title"));
	Objective.Description = Parsed.GetString(TEXT("description"));
	Objective.Status = Parsed.GetString(TEXT("status"));

	if (Objective.ObjectiveId.IsEmpty())
	{
		Objective.ObjectiveId = Parsed.Event.Target;
	}

	if (Objective.Title.IsEmpty())
	{
		Objective.Title = Objective.Description.IsEmpty() ? Objective.ObjectiveId : Objective.Description;
	}

	if (Objective.ObjectiveId.IsEmpty() || Objective.Status.IsEmpty())
	{
		return;
	}

	const FString ObjectiveId = Objective.ObjectiveId;
	ActiveObjectives.RemoveAll([&ObjectiveId](const FRiftObjectiveState& Other)
	{
		return Other.ObjectiveId.Equals(ObjectiveId, ESearchCase::CaseSensitive);
	});

	if (IsStatus(Objective.Status, RiftEvents::Status::Active))
	{
		ActiveObjectives.Add(Objective);

		State.bHasObjective = true;
		State.bObjectiveFailed = false;
		State.CurrentObjective = Objective;
		PendingBanners.Add(TEXT("NEW OBJECTIVE"));
		return;
	}

	const bool bFailed = IsFailure(Objective.Status);

	if (!bFailed && !IsStatus(Objective.Status, RiftEvents::Status::Completed))
	{
		// a status this client does not know: nothing to show
		return;
	}

	PendingBanners.Add(bFailed ? TEXT("OBJECTIVE FAILED") : TEXT("OBJECTIVE COMPLETE"));

	const bool bWasShown = State.bHasObjective && State.CurrentObjective.ObjectiveId.Equals(ObjectiveId, ESearchCase::CaseSensitive);

	if (State.bHasObjective && !bWasShown && !State.bObjectiveFailed)
	{
		// another objective is on screen and still stands
		return;
	}

	if (ActiveObjectives.Num() > 0)
	{
		State.bHasObjective = true;
		State.bObjectiveFailed = false;
		State.CurrentObjective = ActiveObjectives.Last();
	}
	else
	{
		// keep the finished objective on screen until the backend sets a new one
		State.bHasObjective = true;
		State.bObjectiveFailed = bFailed;
		State.CurrentObjective = Objective;
	}
}

void FRiftHudModel::ApplyMission(const FRiftParsedWorldEvent& Parsed)
{
	const FString Status = Parsed.GetString(TEXT("status"));

	if (IsFailure(Status))
	{
		PendingBanners.Add(TEXT("MISSION FAILED"));
	}
	else if (IsStatus(Status, RiftEvents::Status::Completed))
	{
		PendingBanners.Add(TEXT("MISSION COMPLETE"));
	}
	else if (IsStatus(Status, RiftEvents::Status::Active))
	{
		const FString Title = Parsed.GetString(TEXT("title"));
		PendingBanners.Add(Title.IsEmpty() ? FString(TEXT("NEW MISSION")) : FString::Printf(TEXT("NEW MISSION: %s"), *Title));
	}
}

void FRiftHudModel::ShowSubtitle(const FString& SpeakerId, const FString& Line, double Now)
{
	if (Line.IsEmpty())
	{
		return;
	}

	State.SubtitleSpeakerId = SpeakerId;
	State.Subtitle = Line;

	// long lines stay up longer
	SubtitleExpires = Now + 4.0 + 0.06 * Line.Len();
}
