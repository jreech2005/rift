// Automation tests for the HUD model and the voice helpers: run with "Automation RunTests Rift.Presentation".

#include "Misc/AutomationTest.h"
#include "RiftVoice.h"
#include "RiftWorldEventTypes.h"

#if WITH_DEV_AUTOMATION_TESTS

namespace
{
	FRiftParsedWorldEvent MakeEvent(const TCHAR* EventType, const TCHAR* Target, const TCHAR* PayloadJson, int64 Sequence = 0)
	{
		FRiftWorldEvent Event;
		Event.SessionId = TEXT("session");
		Event.Sequence = Sequence;
		Event.EventType = EventType;
		Event.Target = Target;
		Event.PayloadJson = PayloadJson;

		return RiftEvents::Parse(Event);
	}
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FRiftHudModelTest, "Rift.Presentation.HudModel", EAutomationTestFlags_ApplicationContextMask | EAutomationTestFlags::EngineFilter)

bool FRiftHudModelTest::RunTest(const FString& Parameters)
{
	// event types
	TestEqual(TEXT("known type"), RiftEvents::Classify(TEXT("objective_updated")), ERiftWorldEventType::ObjectiveUpdated);
	TestEqual(TEXT("type names are case sensitive"), RiftEvents::Classify(TEXT("Objective_Updated")), ERiftWorldEventType::Unknown);
	TestEqual(TEXT("unknown type"), RiftEvents::Classify(TEXT("weather_changed")), ERiftWorldEventType::Unknown);

	// unknown and malformed events are harmless
	{
		FRiftHudModel Model;

		TestFalse(TEXT("unknown type changes nothing"), Model.Apply(MakeEvent(TEXT("weather_changed"), TEXT("sky"), TEXT("{\"rain\":true}")), 0.0));
		TestFalse(TEXT("acknowledgement changes nothing"), Model.Apply(MakeEvent(TEXT("speech_acknowledged"), TEXT("hank_schrader"), TEXT("{}")), 0.0));

		Model.Apply(MakeEvent(TEXT("objective_updated"), TEXT(""), TEXT("not json")), 0.0);
		Model.Apply(MakeEvent(TEXT("objective_updated"), TEXT("x"), TEXT("{\"status\":7,\"title\":null}")), 0.0);
		Model.Apply(MakeEvent(TEXT("dialogue_started"), TEXT("hank_schrader"), TEXT("")), 0.0);
		Model.Apply(MakeEvent(TEXT("world_flag_changed"), TEXT(""), TEXT("[1,2]")), 0.0);

		TestFalse(TEXT("malformed objective shows nothing"), Model.State.bHasObjective);
		TestTrue(TEXT("malformed events leave the banner empty"), Model.State.Banner.IsEmpty());
		TestTrue(TEXT("malformed dialogue shows no subtitle"), Model.State.Subtitle.IsEmpty());
		TestEqual(TEXT("malformed flag event sets no flag"), Model.WorldFlags.Num(), 0);
	}

	// the burner phone divergence: the opening objective fails, the mission goes with it, the Director sets a new one
	{
		FRiftHudModel Model;

		Model.Apply(MakeEvent(TEXT("mission_updated"), TEXT("protect_walters_cover"),
			TEXT("{\"source\":\"narrative\",\"mission_id\":\"protect_walters_cover\",\"status\":\"failed\",\"title\":\"Protect Walter's cover\"}"), 2), 10.0);
		Model.Apply(MakeEvent(TEXT("objective_updated"), TEXT("hide_burner_phone"),
			TEXT("{\"source\":\"narrative\",\"objective_id\":\"hide_burner_phone\",\"mission_id\":\"protect_walters_cover\",\"status\":\"failed\",\"title\":\"Hide the phone\",\"description\":\"Hide the phone\"}"), 3), 10.0);

		TestEqual(TEXT("first banner"), Model.State.Banner, FString(TEXT("MISSION FAILED")));
		TestTrue(TEXT("failed objective is shown"), Model.State.bHasObjective);
		TestTrue(TEXT("failed objective is marked failed"), Model.State.bObjectiveFailed);
		TestEqual(TEXT("failed objective title"), Model.State.CurrentObjective.Title, FString(TEXT("Hide the phone")));

		TestFalse(TEXT("banner stays while it has time left"), Model.Advance(10.0 + FRiftHudModel::BannerSeconds * 0.5));
		TestTrue(TEXT("queued banner follows"), Model.Advance(10.0 + FRiftHudModel::BannerSeconds));
		TestEqual(TEXT("second banner"), Model.State.Banner, FString(TEXT("OBJECTIVE FAILED")));

		TestFalse(TEXT("a resent event is applied once"), Model.Apply(MakeEvent(TEXT("objective_updated"), TEXT("hide_burner_phone"),
			TEXT("{\"objective_id\":\"hide_burner_phone\",\"status\":\"active\",\"title\":\"Hide the phone\"}"), 3), 14.0));
		TestTrue(TEXT("resent event did not revive the objective"), Model.State.bObjectiveFailed);

		Model.Apply(MakeEvent(TEXT("objective_updated"), TEXT("choose_what_to_tell_hank"),
			TEXT("{\"source\":\"director\",\"objective_id\":\"choose_what_to_tell_hank\",\"mission_id\":null,\"status\":\"active\",\"title\":\"Choose what to tell Hank\",\"description\":\"Hank wants the whole story.\"}"), 7), 20.0);

		TestEqual(TEXT("new objective banner"), Model.State.Banner, FString(TEXT("NEW OBJECTIVE")));
		TestFalse(TEXT("new objective is not failed"), Model.State.bObjectiveFailed);
		TestEqual(TEXT("new objective id"), Model.State.CurrentObjective.ObjectiveId, FString(TEXT("choose_what_to_tell_hank")));

		Model.Advance(100.0);
		TestTrue(TEXT("banner expires"), Model.State.Banner.IsEmpty());
		TestTrue(TEXT("objective outlives its banner"), Model.State.bHasObjective);

		Model.Apply(MakeEvent(TEXT("objective_updated"), TEXT("choose_what_to_tell_hank"),
			TEXT("{\"objective_id\":\"choose_what_to_tell_hank\",\"status\":\"completed\",\"title\":\"Choose what to tell Hank\"}"), 8), 200.0);
		TestEqual(TEXT("completion banner"), Model.State.Banner, FString(TEXT("OBJECTIVE COMPLETE")));
		TestFalse(TEXT("completed is not failed"), Model.State.bObjectiveFailed);
	}

	// flags, dialogue, triggered events
	{
		FRiftHudModel Model;

		Model.Apply(MakeEvent(TEXT("world_flag_changed"), TEXT(""), TEXT("{\"flag\":\"hank_knows_about_phone\",\"value\":true}")), 0.0);
		Model.Apply(MakeEvent(TEXT("world_flag_changed"), TEXT(""), TEXT("{\"flag\":\"door_locked\",\"value\":false}")), 0.0);

		TestTrue(TEXT("flag set"), Model.WorldFlags.FindRef(TEXT("hank_knows_about_phone")));
		TestTrue(TEXT("cleared flag is recorded"), Model.WorldFlags.Contains(TEXT("door_locked")));
		TestFalse(TEXT("cleared flag is false"), Model.WorldFlags.FindRef(TEXT("door_locked")));

		Model.Apply(MakeEvent(TEXT("dialogue_started"), TEXT("hank_schrader"),
			TEXT("{\"source\":\"director\",\"npc_id\":\"hank_schrader\",\"opening_line\":\"You did the right thing telling me.\"}")), 5.0);

		TestEqual(TEXT("subtitle speaker"), Model.State.SubtitleSpeakerId, FString(TEXT("hank_schrader")));
		TestEqual(TEXT("subtitle line"), Model.State.Subtitle, FString(TEXT("You did the right thing telling me.")));

		Model.Apply(MakeEvent(TEXT("information_revealed"), TEXT("walter_white"),
			TEXT("{\"recipient_id\":\"walter_white\",\"source_npc_id\":\"hank_schrader\"}")), 5.0);
		TestEqual(TEXT("information for an NPC is not shown to the player"), Model.State.SubtitleSpeakerId, FString(TEXT("hank_schrader")));

		Model.Apply(MakeEvent(TEXT("world_event_triggered"), TEXT("albuquerque_hospital"),
			TEXT("{\"event\":\"confrontation_begins\",\"description\":\"Hank steps into the room.\",\"location_id\":\"albuquerque_hospital\",\"npc_ids\":[\"hank_schrader\",\"walter_white\"]}")), 5.0);
		TestEqual(TEXT("notice"), Model.State.Notice, FString(TEXT("Hank steps into the room.")));

		const FRiftParsedWorldEvent Triggered = MakeEvent(TEXT("world_event_triggered"), TEXT(""), TEXT("{\"npc_ids\":[\"a\",\"b\"]}"));
		TestEqual(TEXT("npc_ids are read"), Triggered.GetStringArray(TEXT("npc_ids")).Num(), 2);

		Model.Advance(1000.0);
		TestTrue(TEXT("subtitle expires"), Model.State.Subtitle.IsEmpty());
		TestTrue(TEXT("notice expires"), Model.State.Notice.IsEmpty());
	}

	return true;
}

IMPLEMENT_SIMPLE_AUTOMATION_TEST(FRiftVoiceTest, "Rift.Presentation.Voice", EAutomationTestFlags_ApplicationContextMask | EAutomationTestFlags::EngineFilter)

bool FRiftVoiceTest::RunTest(const FString& Parameters)
{
	// audio_url is a path on the backend that owns the socket
	TestEqual(TEXT("ws becomes http"), RiftVoice::ResolveAudioUrl(TEXT("ws://127.0.0.1:3000/ws"), TEXT("/audio/abc")), FString(TEXT("http://127.0.0.1:3000/audio/abc")));
	TestEqual(TEXT("wss becomes https"), RiftVoice::ResolveAudioUrl(TEXT("wss://rift.example/ws"), TEXT("/audio/abc")), FString(TEXT("https://rift.example/audio/abc")));
	TestEqual(TEXT("backend url without a path"), RiftVoice::ResolveAudioUrl(TEXT("ws://localhost:3000"), TEXT("/audio/abc")), FString(TEXT("http://localhost:3000/audio/abc")));
	TestTrue(TEXT("a full url is refused"), RiftVoice::ResolveAudioUrl(TEXT("ws://127.0.0.1:3000/ws"), TEXT("http://elsewhere/audio/abc")).IsEmpty());
	TestTrue(TEXT("a //host reference is refused"), RiftVoice::ResolveAudioUrl(TEXT("ws://127.0.0.1:3000/ws"), TEXT("//elsewhere/audio/abc")).IsEmpty());
	TestTrue(TEXT("no audio_url, no request"), RiftVoice::ResolveAudioUrl(TEXT("ws://127.0.0.1:3000/ws"), TEXT("")).IsEmpty());
	TestTrue(TEXT("unknown backend scheme"), RiftVoice::ResolveAudioUrl(TEXT("ftp://127.0.0.1/ws"), TEXT("/audio/abc")).IsEmpty());

	// the 44 byte header the backend writes for pcm_24000, then four samples
	TArray<uint8> Wav = {
		'R', 'I', 'F', 'F', 44, 0, 0, 0, 'W', 'A', 'V', 'E',
		'f', 'm', 't', ' ', 16, 0, 0, 0, 1, 0, 1, 0, 0xC0, 0x5D, 0, 0, 0x80, 0xBB, 0, 0, 2, 0, 16, 0,
		'd', 'a', 't', 'a', 8, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8 };

	FRiftWavClip Clip;
	TestTrue(TEXT("pcm wav parses"), RiftVoice::ParseWav(Wav, Clip));
	TestEqual(TEXT("sample rate"), Clip.SampleRate, 24000);
	TestEqual(TEXT("channels"), Clip.NumChannels, 1);
	TestEqual(TEXT("pcm size"), Clip.Pcm.Num(), 8);
	TestEqual(TEXT("first pcm byte"), static_cast<int32>(Clip.Pcm[0]), 1);
	TestEqual(TEXT("duration"), Clip.GetDuration(), 4.0f / 24000.0f);

	// a data chunk that claims more than was received is cut to whole samples
	TArray<uint8> Truncated = Wav;
	Truncated.SetNum(Wav.Num() - 3);
	TestTrue(TEXT("truncated wav still parses"), RiftVoice::ParseWav(Truncated, Clip));
	TestEqual(TEXT("truncated pcm keeps whole samples"), Clip.Pcm.Num(), 4);

	const TArray<uint8> Mp3 = { 'I', 'D', '3', 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 };
	TestFalse(TEXT("mp3 is refused"), RiftVoice::ParseWav(Mp3, Clip));
	TestFalse(TEXT("empty is refused"), RiftVoice::ParseWav(TArray<uint8>(), Clip));

	TArray<uint8> Float32 = Wav;
	Float32[20] = 3;
	TestFalse(TEXT("non PCM encoding is refused"), RiftVoice::ParseWav(Float32, Clip));

	TArray<uint8> HeaderOnly = Wav;
	HeaderOnly.SetNum(44);
	TestFalse(TEXT("a clip without samples is refused"), RiftVoice::ParseWav(HeaderOnly, Clip));

	return true;
}

#endif
