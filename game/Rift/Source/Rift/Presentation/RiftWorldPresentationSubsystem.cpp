// Turns validated backend world events into visible game behavior.

#include "RiftWorldPresentationSubsystem.h"

#include "Components/AudioComponent.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "HttpModule.h"
#include "Interfaces/IHttpRequest.h"
#include "Interfaces/IHttpResponse.h"
#include "Kismet/GameplayStatics.h"
#include "RiftEntityComponent.h"
#include "RiftLocationMarker.h"
#include "RiftNPC.h"
#include "RiftProtocol.h"
#include "RiftVoice.h"
#include "Sound/SoundWaveProcedural.h"

bool URiftWorldPresentationSubsystem::DoesSupportWorldType(const EWorldType::Type WorldType) const
{
	return WorldType == EWorldType::Game || WorldType == EWorldType::PIE;
}

void URiftWorldPresentationSubsystem::OnWorldBeginPlay(UWorld& InWorld)
{
	Super::OnWorldBeginPlay(InWorld);

	if (URiftNetworkSubsystem* Network = GetNetwork())
	{
		Network->OnWorldEvent.AddDynamic(this, &URiftWorldPresentationSubsystem::HandleWorldEvent);
		Network->OnReady.AddDynamic(this, &URiftWorldPresentationSubsystem::HandleReady);
		Network->OnDisconnected.AddDynamic(this, &URiftWorldPresentationSubsystem::HandleDisconnected);
		Network->OnSessionCreated.AddDynamic(this, &URiftWorldPresentationSubsystem::HandleSessionCreated);
		Network->OnBackendError.AddDynamic(this, &URiftWorldPresentationSubsystem::HandleBackendError);
	}

	CreateSessionIfNeeded();
}

void URiftWorldPresentationSubsystem::Deinitialize()
{
	if (URiftNetworkSubsystem* Network = GetNetwork())
	{
		Network->OnWorldEvent.RemoveAll(this);
		Network->OnReady.RemoveAll(this);
		Network->OnDisconnected.RemoveAll(this);
		Network->OnSessionCreated.RemoveAll(this);
		Network->OnBackendError.RemoveAll(this);
	}

	Entities.Reset();

	// a clip still on its way is dropped when it arrives
	++VoiceRequestId;
	StopVoice();

	Super::Deinitialize();
}

void URiftWorldPresentationSubsystem::Tick(float DeltaTime)
{
	Super::Tick(DeltaTime);

	if (HudModel.Advance(Now()))
	{
		OnHudChanged.Broadcast(HudModel.State);
	}

	// a procedural wave never ends on its own
	if (VoiceComponent && Now() >= VoiceEnds)
	{
		StopVoice();
	}
}

TStatId URiftWorldPresentationSubsystem::GetStatId() const
{
	RETURN_QUICK_DECLARE_CYCLE_STAT(URiftWorldPresentationSubsystem, STATGROUP_Tickables);
}

void URiftWorldPresentationSubsystem::RegisterEntity(URiftEntityComponent* Entity)
{
	if (!Entity || Entity->RiftId.IsEmpty())
	{
		UE_LOG(LogRiftPresentation, Warning, TEXT("%s has a Rift Entity component without a Rift id"), Entity ? *GetNameSafe(Entity->GetOwner()) : TEXT("null"));
		return;
	}

	if (const URiftEntityComponent* Existing = FindEntity(Entity->RiftId))
	{
		if (Existing != Entity)
		{
			UE_LOG(LogRiftPresentation, Warning, TEXT("Rift id '%s' is used by both %s and %s, the later one wins"),
				*Entity->RiftId, *GetNameSafe(Existing->GetOwner()), *GetNameSafe(Entity->GetOwner()));
		}
	}

	Entities.Add(Entity->RiftId, Entity);

	UE_LOG(LogRiftPresentation, Log, TEXT("Rift entity '%s' registered (%s)"), *Entity->RiftId, *GetNameSafe(Entity->GetOwner()));

	CreateSessionIfNeeded();
}

void URiftWorldPresentationSubsystem::UnregisterEntity(URiftEntityComponent* Entity)
{
	if (Entity && FindEntity(Entity->RiftId) == Entity)
	{
		Entities.Remove(Entity->RiftId);
	}
}

URiftEntityComponent* URiftWorldPresentationSubsystem::FindEntity(const FString& RiftId) const
{
	// TMap compares FString keys without case, Rift ids are case sensitive
	const TWeakObjectPtr<URiftEntityComponent>* Found = Entities.Find(RiftId);
	URiftEntityComponent* Entity = Found ? Found->Get() : nullptr;

	return Entity && Entity->RiftId.Equals(RiftId, ESearchCase::CaseSensitive) ? Entity : nullptr;
}

FString URiftWorldPresentationSubsystem::GetDisplayName(const FString& RiftId) const
{
	const URiftEntityComponent* Entity = FindEntity(RiftId);
	return Entity ? Entity->GetDisplayNameOrId() : RiftId;
}

bool URiftWorldPresentationSubsystem::GetWorldFlag(const FString& Flag) const
{
	const bool* Value = HudModel.WorldFlags.Find(Flag);
	return Value && *Value;
}

FString URiftWorldPresentationSubsystem::GetPayloadString(const FRiftWorldEvent& Event, const FString& Field)
{
	return RiftEvents::Parse(Event).GetString(*Field);
}

void URiftWorldPresentationSubsystem::PresentLocalEvent(const FString& EventType, const FString& Target, const FString& PayloadJson)
{
	FRiftWorldEvent Event;
	Event.EventId = RiftProtocol::NewId();
	Event.EventType = EventType;
	Event.Target = Target;
	Event.PayloadJson = PayloadJson;
	Event.Timestamp = RiftProtocol::NowTimestamp();

	UE_LOG(LogRiftPresentation, Display, TEXT("Presenting local event %s -> %s %s"), *EventType, *Target, *PayloadJson);

	HandleWorldEvent(Event);
}

void URiftWorldPresentationSubsystem::HandleWorldEvent(const FRiftWorldEvent& Event)
{
	const FRiftParsedWorldEvent Parsed = RiftEvents::Parse(Event);

	if (Parsed.Type == ERiftWorldEventType::Unknown)
	{
		// V1 may add event types, an older client shows nothing for them
		UE_LOG(LogRiftPresentation, Log, TEXT("No presentation for world_event type '%s'"), *Event.EventType);
	}

	const bool bHudChanged = HudModel.Apply(Parsed, Now());

	switch (Parsed.Type)
	{
	case ERiftWorldEventType::ObjectiveUpdated:
	{
		FRiftObjectiveState Objective;
		Objective.ObjectiveId = Parsed.GetString(TEXT("objective_id"));
		Objective.MissionId = Parsed.GetString(TEXT("mission_id"));
		Objective.Title = Parsed.GetString(TEXT("title"));
		Objective.Description = Parsed.GetString(TEXT("description"));
		Objective.Status = Parsed.GetString(TEXT("status"));
		OnObjectiveUpdated.Broadcast(Objective);
		break;
	}

	case ERiftWorldEventType::MissionUpdated:
		OnMissionUpdated.Broadcast(Parsed.GetString(TEXT("mission_id")), Parsed.GetString(TEXT("status")), Parsed.GetString(TEXT("title")));
		break;

	case ERiftWorldEventType::WorldFlagChanged:
	{
		const FString Flag = Parsed.GetString(TEXT("flag"));

		if (!Flag.IsEmpty())
		{
			OnWorldFlagChanged.Broadcast(Flag, GetWorldFlag(Flag));
		}
		break;
	}

	case ERiftWorldEventType::WorldEventTriggered:
		OnWorldEventTriggered.Broadcast(Parsed.GetString(TEXT("event")), Parsed.GetString(TEXT("description")));
		break;

	default:
		break;
	}

	if (bHudChanged)
	{
		if (!HudModel.State.Subtitle.IsEmpty() &&
			(Parsed.Type == ERiftWorldEventType::DialogueStarted || Parsed.Type == ERiftWorldEventType::InformationRevealed))
		{
			OnDialogueLine.Broadcast(HudModel.State.SubtitleSpeakerId, HudModel.State.Subtitle);
		}

		OnHudChanged.Broadcast(HudModel.State);

		// the subtitle is already up, the voice joins it when the clip arrives
		if (Parsed.Type == ERiftWorldEventType::DialogueStarted)
		{
			RequestVoice(Parsed.GetString(TEXT("audio_url")));
		}
	}

	RouteToWorld(Parsed);

	OnAnyWorldEvent.Broadcast(Event);
}

void URiftWorldPresentationSubsystem::RouteToWorld(const FRiftParsedWorldEvent& Parsed)
{
	// every entity the event names hears about it once
	TArray<FString> Named;
	Named.AddUnique(Parsed.Event.Target);
	Named.AddUnique(Parsed.GetString(TEXT("npc_id")));

	const TArray<FString> NpcIds = Parsed.GetStringArray(TEXT("npc_ids"));

	for (const FString& NpcId : NpcIds)
	{
		Named.AddUnique(NpcId);
	}

	switch (Parsed.Type)
	{
	case ERiftWorldEventType::NpcMoved:
	case ERiftWorldEventType::NpcActivated:
	{
		const FString NpcId = Parsed.GetString(TEXT("npc_id"));
		MoveNpcToMarker(NpcId.IsEmpty() ? Parsed.Event.Target : NpcId, Parsed.GetString(TEXT("location_id")));
		break;
	}

	case ERiftWorldEventType::WorldEventTriggered:
	{
		// a triggered event can stage its NPCs: markers named after the event say where each one stands
		const FString EventName = Parsed.GetString(TEXT("event"));

		for (const FString& NpcId : NpcIds)
		{
			MoveNpcToMarker(NpcId, EventName);
		}
		break;
	}

	case ERiftWorldEventType::DialogueStarted:
	{
		const FString NpcId = Parsed.GetString(TEXT("npc_id"));

		if (ARiftNPC* Npc = FindNpc(NpcId.IsEmpty() ? Parsed.Event.Target : NpcId))
		{
			Npc->PresentDialogue(Parsed.GetString(TEXT("opening_line")));
		}
		break;
	}

	case ERiftWorldEventType::SpeechAcknowledged:
		// the player spoke to this NPC: it turns to listen
		if (ARiftNPC* Npc = FindNpc(Parsed.Event.Target))
		{
			Npc->FacePlayer();
		}
		break;

	default:
		break;
	}

	for (const FString& RiftId : Named)
	{
		if (URiftEntityComponent* Entity = RiftId.IsEmpty() ? nullptr : FindEntity(RiftId))
		{
			Entity->OnRiftEvent.Broadcast(Parsed.Event);
		}
	}
}

ARiftNPC* URiftWorldPresentationSubsystem::FindNpc(const FString& NpcId) const
{
	const URiftEntityComponent* Entity = NpcId.IsEmpty() ? nullptr : FindEntity(NpcId);
	return Entity ? Cast<ARiftNPC>(Entity->GetOwner()) : nullptr;
}

void URiftWorldPresentationSubsystem::MoveNpcToMarker(const FString& NpcId, const FString& MarkerId)
{
	ARiftNPC* Npc = FindNpc(NpcId);
	ARiftLocationMarker* Marker = Npc ? ARiftLocationMarker::Find(GetWorld(), MarkerId, NpcId) : nullptr;

	if (!Marker)
	{
		UE_LOG(LogRiftPresentation, Verbose, TEXT("Nothing to move for npc '%s' and marker '%s'"), *NpcId, *MarkerId);
		return;
	}

	UE_LOG(LogRiftPresentation, Log, TEXT("%s walks to marker %s"), *NpcId, *MarkerId);

	Npc->MoveToMarker(Marker);
}

void URiftWorldPresentationSubsystem::HandleReady()
{
	CreateSessionIfNeeded();
}

void URiftWorldPresentationSubsystem::HandleDisconnected(const FString& Reason)
{
}

void URiftWorldPresentationSubsystem::HandleSessionCreated(const FString& SessionId)
{
	// a new session starts a new story, nothing of the old one stays on screen
	++VoiceRequestId;
	StopVoice();
	HudModel = FRiftHudModel();
	OnHudChanged.Broadcast(HudModel.State);
}

void URiftWorldPresentationSubsystem::HandleBackendError(const FString& Code, const FString& Message)
{
	// the network layer drops a session the backend no longer knows, ask for a new one
	CreateSessionIfNeeded();
}

URiftNetworkSubsystem* URiftWorldPresentationSubsystem::GetNetwork() const
{
	const UWorld* World = GetWorld();
	const UGameInstance* GameInstance = World ? World->GetGameInstance() : nullptr;

	return GameInstance ? GameInstance->GetSubsystem<URiftNetworkSubsystem>() : nullptr;
}

void URiftWorldPresentationSubsystem::CreateSessionIfNeeded()
{
	if (!bAutoCreateSession || Entities.Num() == 0)
	{
		return;
	}

	// the network layer normally has a session by now, it sends nothing if one exists or is on its way
	if (URiftNetworkSubsystem* Network = GetNetwork())
	{
		Network->EnsureSession();
	}
}

void URiftWorldPresentationSubsystem::RequestVoice(const FString& AudioUrl)
{
	if (AudioUrl.IsEmpty() || !bPlayVoice)
	{
		return;
	}

	const URiftNetworkSubsystem* Network = GetNetwork();
	const FString Url = Network ? RiftVoice::ResolveAudioUrl(Network->GetBackendUrl(), AudioUrl) : FString();

	if (Url.IsEmpty())
	{
		UE_LOG(LogRiftPresentation, Warning, TEXT("Voice: audio_url '%s' is not a path on the backend, line stays text only"), *AudioUrl);
		return;
	}

	const int32 RequestId = ++VoiceRequestId;

	const TSharedRef<IHttpRequest, ESPMode::ThreadSafe> Request = FHttpModule::Get().CreateRequest();
	Request->SetVerb(TEXT("GET"));
	Request->SetURL(Url);
	Request->SetTimeout(10.0f);

	// completes on the game thread, the game never waits for it
	Request->OnProcessRequestComplete().BindWeakLambda(this, [this, RequestId, Url](FHttpRequestPtr, FHttpResponsePtr Response, bool bConnected)
	{
		if (!bConnected || !Response.IsValid() || Response->GetResponseCode() != 200)
		{
			UE_LOG(LogRiftPresentation, Warning, TEXT("Voice: could not fetch %s (HTTP %d), line stays text only"), *Url, Response.IsValid() ? Response->GetResponseCode() : 0);
			return;
		}

		PlayVoiceClip(RequestId, Response->GetContent());
	});

	if (!Request->ProcessRequest())
	{
		UE_LOG(LogRiftPresentation, Warning, TEXT("Voice: could not start the request for %s"), *Url);
	}
}

void URiftWorldPresentationSubsystem::PlayVoiceClip(int32 RequestId, const TArray<uint8>& Bytes)
{
	if (RequestId != VoiceRequestId)
	{
		// a newer line was requested while this one was on its way
		return;
	}

	FRiftWavClip Clip;

	if (!RiftVoice::ParseWav(Bytes, Clip))
	{
		UE_LOG(LogRiftPresentation, Warning, TEXT("Voice: clip is not 16 bit PCM WAV (%d bytes). Set ELEVENLABS_OUTPUT_FORMAT=pcm_24000 on the backend"), Bytes.Num());
		return;
	}

	StopVoice();

	USoundWaveProcedural* Wave = NewObject<USoundWaveProcedural>(this);
	Wave->SetSampleRate(Clip.SampleRate);
	Wave->NumChannels = Clip.NumChannels;
	Wave->Duration = Clip.GetDuration();
	Wave->SoundGroup = SOUNDGROUP_Voice;
	Wave->bLooping = false;
	Wave->QueueAudio(Clip.Pcm.GetData(), Clip.Pcm.Num());

	// null without an audio device, for example a headless run
	VoiceComponent = UGameplayStatics::SpawnSound2D(this, Wave, VoiceVolume, 1.0f, 0.0f, nullptr, false, false);
	VoiceEnds = Now() + Clip.GetDuration() + 0.25;

	UE_LOG(LogRiftPresentation, Log, TEXT("Voice: playing %.1f s at %d Hz%s"), Clip.GetDuration(), Clip.SampleRate, VoiceComponent ? TEXT("") : TEXT(" (no audio device)"));
}

void URiftWorldPresentationSubsystem::StopVoice()
{
	if (VoiceComponent)
	{
		VoiceComponent->Stop();
		VoiceComponent->DestroyComponent();
		VoiceComponent = nullptr;
	}
}

double URiftWorldPresentationSubsystem::Now() const
{
	const UWorld* World = GetWorld();
	return World ? World->GetTimeSeconds() : 0.0;
}
