// Live acceptance run of the backend connection, started with the Rift.NetSmoke console command.

#pragma once

#include "CoreMinimal.h"
#include "Containers/Ticker.h"
#include "UObject/Object.h"
#include "RiftNetSmokeTest.generated.h"

class URiftNetworkSubsystem;
struct FRiftWorldEvent;

/**
 *  Drives hello, ping, create_session and a test player_action against the real backend
 *  and logs PASS or FAIL. Uses only the public surface of URiftNetworkSubsystem.
 *  Event driven: each reply starts the next step, one timeout guards the whole run.
 */
UCLASS()
class URiftNetSmokeTest : public UObject
{
	GENERATED_BODY()

public:

	/**
	 *  Starts the run. The object keeps itself alive until it finishes.
	 *  @param bInExitWhenDone	requests engine exit after the result is logged, for headless runs
	 */
	void Start(URiftNetworkSubsystem* InNetwork, bool bInExitWhenDone);

private:

	enum class EStep : uint8
	{
		Idle,
		Hello,
		Ping,
		Session,
		Action,
		DuplicateAction,
		Done
	};

	UFUNCTION()
	void HandleReady();

	UFUNCTION()
	void HandleDisconnected(const FString& Reason);

	UFUNCTION()
	void HandlePong(const FString& Nonce);

	UFUNCTION()
	void HandleSessionCreated(const FString& InSessionId);

	UFUNCTION()
	void HandleWorldEvent(const FRiftWorldEvent& Event);

	UFUNCTION()
	void HandleBackendError(const FString& Code, const FString& Message);

	/** One shot, fails the run if a reply never arrives */
	bool HandleTimeout(float DeltaTime);

	/** Checks that malformed and mismatched frames are rejected by the parser */
	bool CheckBadFramesRejected(FString& OutFailure) const;

	void Pass();
	void Fail(const FString& Reason);
	void Finish();

	const TCHAR* GetStepName() const;

	UPROPERTY()
	TObjectPtr<URiftNetworkSubsystem> Network;

	FTSTicker::FDelegateHandle TimeoutHandle;

	EStep Step = EStep::Idle;

	bool bExitWhenDone = false;

	FString PingNonce;
	FString SessionId;
	FString DuplicateActionId;

	/** What the test action came back as */
	FString EventType;
	FString EventTarget;
};
