// Live acceptance run of the backend connection, started with the Rift.NetSmoke console command.

#include "RiftNetSmokeTest.h"

#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "HAL/IConsoleManager.h"
#include "RiftNetworkSubsystem.h"
#include "RiftProtocol.h"

namespace
{
	const float TimeoutSeconds = 10.0f;

	const TCHAR* const ExpectedEventType = TEXT("interaction_acknowledged");
	const TCHAR* const ExpectedEventTarget = TEXT("test_door");
	const TCHAR* const DuplicateErrorCode = TEXT("duplicate_action");

	void RunNetSmoke(const TArray<FString>& Args, UWorld* World)
	{
		UGameInstance* GameInstance = World ? World->GetGameInstance() : nullptr;
		URiftNetworkSubsystem* Network = GameInstance ? GameInstance->GetSubsystem<URiftNetworkSubsystem>() : nullptr;

		if (!Network)
		{
			UE_LOG(LogRiftNet, Warning, TEXT("Rift.NetSmoke needs a running game: start Play In Editor or launch with -game"));
			return;
		}

		NewObject<URiftNetSmokeTest>(Network)->Start(Network, Args.Contains(TEXT("exit")));
	}

	FAutoConsoleCommandWithWorldAndArgs NetSmokeCommand(
		TEXT("Rift.NetSmoke"),
		TEXT("Runs hello, ping, create_session and a test player_action against the Rift backend and logs PASS or FAIL. Add 'exit' to quit afterwards."),
		FConsoleCommandWithWorldAndArgsDelegate::CreateStatic(&RunNetSmoke));
}

void URiftNetSmokeTest::Start(URiftNetworkSubsystem* InNetwork, bool bInExitWhenDone)
{
	Network = InNetwork;
	bExitWhenDone = bInExitWhenDone;

	// nothing else references this object while replies are pending
	AddToRoot();

	Network->OnReady.AddDynamic(this, &URiftNetSmokeTest::HandleReady);
	Network->OnDisconnected.AddDynamic(this, &URiftNetSmokeTest::HandleDisconnected);
	Network->OnPong.AddDynamic(this, &URiftNetSmokeTest::HandlePong);
	Network->OnSessionCreated.AddDynamic(this, &URiftNetSmokeTest::HandleSessionCreated);
	Network->OnWorldEvent.AddDynamic(this, &URiftNetSmokeTest::HandleWorldEvent);
	Network->OnBackendError.AddDynamic(this, &URiftNetSmokeTest::HandleBackendError);

	TimeoutHandle = FTSTicker::GetCoreTicker().AddTicker(FTickerDelegate::CreateUObject(this, &URiftNetSmokeTest::HandleTimeout), TimeoutSeconds);

	UE_LOG(LogRiftNet, Display, TEXT("Rift.NetSmoke: starting against %s"), *Network->GetBackendUrl());

	Step = EStep::Hello;

	if (Network->IsConnected())
	{
		HandleReady();
	}
	else
	{
		// does nothing if a connection attempt is already under way
		Network->Connect();
	}
}

void URiftNetSmokeTest::HandleReady()
{
	if (Step != EStep::Hello)
	{
		return;
	}

	UE_LOG(LogRiftNet, Display, TEXT("Rift.NetSmoke: ok hello -> hello_ack"));

	Step = EStep::Ping;
	PingNonce = RiftProtocol::NewId();

	if (!Network->SendPing(PingNonce))
	{
		Fail(TEXT("could not send ping"));
	}
}

void URiftNetSmokeTest::HandleDisconnected(const FString& Reason)
{
	if (Step != EStep::Done)
	{
		Fail(Reason);
	}
}

void URiftNetSmokeTest::HandlePong(const FString& Nonce)
{
	if (Step != EStep::Ping)
	{
		return;
	}

	if (!Nonce.Equals(PingNonce, ESearchCase::CaseSensitive))
	{
		Fail(FString::Printf(TEXT("pong nonce '%s' does not match '%s'"), *Nonce, *PingNonce));
		return;
	}

	UE_LOG(LogRiftNet, Display, TEXT("Rift.NetSmoke: ok ping -> pong (nonce echoed)"));

	Step = EStep::Session;

	if (!Network->CreateSession())
	{
		Fail(TEXT("could not send create_session"));
	}
}

void URiftNetSmokeTest::HandleSessionCreated(const FString& InSessionId)
{
	if (Step != EStep::Session)
	{
		return;
	}

	if (InSessionId.IsEmpty() || !Network->GetSessionId().Equals(InSessionId, ESearchCase::CaseSensitive))
	{
		Fail(TEXT("session id was not stored"));
		return;
	}

	SessionId = InSessionId;

	UE_LOG(LogRiftNet, Display, TEXT("Rift.NetSmoke: ok create_session -> session_created (%s)"), *SessionId);

	Step = EStep::Action;

	if (!Network->SendTestAction())
	{
		Fail(TEXT("could not send player_action"));
	}
}

void URiftNetSmokeTest::HandleWorldEvent(const FRiftWorldEvent& Event)
{
	if (Step != EStep::Action)
	{
		// the first send of the duplicate pair also answers with an event
		return;
	}

	const bool bExpectedEvent =
		Event.EventType.Equals(ExpectedEventType, ESearchCase::CaseSensitive) &&
		Event.Target.Equals(ExpectedEventTarget, ESearchCase::CaseSensitive) &&
		Event.SessionId.Equals(SessionId, ESearchCase::CaseSensitive);

	if (!bExpectedEvent)
	{
		Fail(FString::Printf(TEXT("unexpected world_event %s -> %s in session %s"), *Event.EventType, *Event.Target, *Event.SessionId));
		return;
	}

	EventType = Event.EventType;
	EventTarget = Event.Target;

	UE_LOG(LogRiftNet, Display, TEXT("Rift.NetSmoke: ok player_action -> world_event (%s -> %s)"), *EventType, *EventTarget);

	// send one action twice: the backend must answer the second with an error message
	Step = EStep::DuplicateAction;
	DuplicateActionId = RiftProtocol::NewId();

	const bool bFirstSent = Network->SendPlayerAction(TEXT("inspect"), ExpectedEventTarget, FString(), DuplicateActionId);
	const bool bSecondSent = Network->SendPlayerAction(TEXT("inspect"), ExpectedEventTarget, FString(), DuplicateActionId);

	if (!bFirstSent || !bSecondSent)
	{
		Fail(TEXT("could not send the duplicate player_action pair"));
	}
}

void URiftNetSmokeTest::HandleBackendError(const FString& Code, const FString& Message)
{
	if (Step == EStep::Done)
	{
		return;
	}

	if (Step != EStep::DuplicateAction || !Code.Equals(DuplicateErrorCode, ESearchCase::CaseSensitive))
	{
		Fail(FString::Printf(TEXT("backend error '%s': %s"), *Code, *Message));
		return;
	}

	if (!Network->IsConnected())
	{
		Fail(TEXT("connection did not survive a backend error"));
		return;
	}

	UE_LOG(LogRiftNet, Display, TEXT("Rift.NetSmoke: ok repeated action_id -> error %s, still connected"), *Code);

	FString BadFrameFailure;

	if (!CheckBadFramesRejected(BadFrameFailure))
	{
		Fail(BadFrameFailure);
		return;
	}

	UE_LOG(LogRiftNet, Display, TEXT("Rift.NetSmoke: ok malformed and wrong-version frames are rejected"));

	Pass();
}

bool URiftNetSmokeTest::HandleTimeout(float DeltaTime)
{
	// the ticker drops this delegate when we return false
	TimeoutHandle.Reset();

	if (Step != EStep::Done)
	{
		Fail(FString::Printf(TEXT("no reply within %.0f seconds"), TimeoutSeconds));
	}

	return false;
}

bool URiftNetSmokeTest::CheckBadFramesRejected(FString& OutFailure) const
{
	static const TCHAR* const BadFrames[] =
	{
		TEXT("{not json"),
		TEXT("[1,2,3]"),
		TEXT("{\"message_id\":\"m\",\"message_type\":\"pong\",\"payload\":{}}"),
		TEXT("{\"protocol_version\":2,\"message_id\":\"m\",\"message_type\":\"pong\",\"payload\":{}}"),
		TEXT("{\"protocol_version\":1,\"message_id\":\"m\",\"payload\":{}}")
	};

	for (const TCHAR* BadFrame : BadFrames)
	{
		FRiftEnvelope Envelope;
		FString ParseError;

		if (RiftProtocol::ParseEnvelope(BadFrame, Envelope, ParseError))
		{
			OutFailure = FString::Printf(TEXT("parser accepted a bad frame: %s"), BadFrame);
			return false;
		}
	}

	return true;
}

void URiftNetSmokeTest::Pass()
{
	UE_LOG(LogRiftNet, Display, TEXT("Rift backend connection: PASS"));
	UE_LOG(LogRiftNet, Display, TEXT("Session: %s"), *SessionId);
	UE_LOG(LogRiftNet, Display, TEXT("WorldEvent: %s -> %s"), *EventType, *EventTarget);

	Finish();
}

void URiftNetSmokeTest::Fail(const FString& Reason)
{
	UE_LOG(LogRiftNet, Error, TEXT("Rift backend connection: FAIL (%s: %s)"), GetStepName(), *Reason);

	Finish();
}

void URiftNetSmokeTest::Finish()
{
	Step = EStep::Done;

	if (TimeoutHandle.IsValid())
	{
		FTSTicker::RemoveTicker(TimeoutHandle);
		TimeoutHandle.Reset();
	}

	if (Network)
	{
		Network->OnReady.RemoveAll(this);
		Network->OnDisconnected.RemoveAll(this);
		Network->OnPong.RemoveAll(this);
		Network->OnSessionCreated.RemoveAll(this);
		Network->OnWorldEvent.RemoveAll(this);
		Network->OnBackendError.RemoveAll(this);
		Network = nullptr;
	}

	RemoveFromRoot();

	if (bExitWhenDone)
	{
		RequestEngineExit(TEXT("Rift.NetSmoke finished"));
	}
}

const TCHAR* URiftNetSmokeTest::GetStepName() const
{
	switch (Step)
	{
	case EStep::Hello:
		return TEXT("hello");
	case EStep::Ping:
		return TEXT("ping");
	case EStep::Session:
		return TEXT("create_session");
	case EStep::Action:
		return TEXT("player_action");
	case EStep::DuplicateAction:
		return TEXT("duplicate player_action");
	default:
		return TEXT("idle");
	}
}
