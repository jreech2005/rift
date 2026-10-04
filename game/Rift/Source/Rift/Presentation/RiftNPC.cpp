// A character the backend can address by Rift id: it can face the player and walk to a marker.

#include "RiftNPC.h"

#include "AIController.h"
#include "Components/TextRenderComponent.h"
#include "GameFramework/CharacterMovementComponent.h"
#include "GameFramework/PlayerController.h"
#include "Navigation/PathFollowingComponent.h"
#include "RiftEntityComponent.h"
#include "RiftLocationMarker.h"
#include "RiftWorldEventTypes.h"

ARiftNPC::ARiftNPC()
{
	PrimaryActorTick.bCanEverTick = true;

	RiftEntity = CreateDefaultSubobject<URiftEntityComponent>(TEXT("RiftEntity"));

	NameTag = CreateDefaultSubobject<UTextRenderComponent>(TEXT("NameTag"));
	NameTag->SetupAttachment(RootComponent);
	NameTag->SetRelativeLocation(FVector(0.0f, 0.0f, 110.0f));
	NameTag->SetHorizontalAlignment(EHTA_Center);
	NameTag->SetWorldSize(22.0f);
	NameTag->SetCollisionEnabled(ECollisionEnabled::NoCollision);

	// a plain AI controller is enough: it owns the path following component
	AIControllerClass = AAIController::StaticClass();
	AutoPossessAI = EAutoPossessAI::PlacedInWorldOrSpawned;

	bUseControllerRotationYaw = false;
	GetCharacterMovement()->bOrientRotationToMovement = true;
	GetCharacterMovement()->MaxWalkSpeed = 220.0f;
}

void ARiftNPC::BeginPlay()
{
	Super::BeginPlay();

	NameTag->SetText(FText::FromString(RiftEntity->GetDisplayNameOrId()));
}

void ARiftNPC::Tick(float DeltaSeconds)
{
	Super::Tick(DeltaSeconds);

	const APlayerController* Player = GetWorld()->GetFirstPlayerController();

	if (!Player)
	{
		return;
	}

	FVector ViewLocation;
	FRotator ViewRotation;
	Player->GetPlayerViewPoint(ViewLocation, ViewRotation);

	// the name is readable from wherever the player stands
	const FVector ToViewer = ViewLocation - NameTag->GetComponentLocation();
	NameTag->SetWorldRotation(FRotator(0.0f, ToViewer.Rotation().Yaw, 0.0f));

	if (bWalkingToMarker)
	{
		const AAIController* AI = Cast<AAIController>(GetController());

		if (!AI || AI->GetMoveStatus() == EPathFollowingStatus::Idle)
		{
			bWalkingToMarker = false;
			bFacingPlayer = bFacePlayerOnArrival;
		}
	}
	else if (bFacingPlayer)
	{
		const FVector ToPlayer = ViewLocation - GetActorLocation();
		const FRotator Target(0.0f, ToPlayer.Rotation().Yaw, 0.0f);

		SetActorRotation(FMath::RInterpConstantTo(GetActorRotation(), Target, DeltaSeconds, FaceTurnSpeed));
	}
}

void ARiftNPC::FacePlayer()
{
	bFacingPlayer = true;
}

bool ARiftNPC::MoveToMarker(ARiftLocationMarker* Marker)
{
	if (!Marker)
	{
		return false;
	}

	AAIController* AI = Cast<AAIController>(GetController());

	if (!AI)
	{
		UE_LOG(LogRiftPresentation, Warning, TEXT("%s cannot move: it has no AI controller"), *GetName());
		return false;
	}

	const EPathFollowingRequestResult::Type Result = AI->MoveToActor(Marker, MarkerAcceptanceRadius);

	if (Result == EPathFollowingRequestResult::Failed)
	{
		// no teleport fallback: a missing NavMesh should be noticed and fixed in the level
		UE_LOG(LogRiftPresentation, Warning, TEXT("%s found no path to marker %s. Is there a NavMesh covering both?"), *GetName(), *Marker->MarkerId);
		return false;
	}

	bFacingPlayer = false;
	bWalkingToMarker = Result == EPathFollowingRequestResult::RequestSuccessful;

	if (!bWalkingToMarker)
	{
		// already standing on the marker
		bFacingPlayer = bFacePlayerOnArrival;
	}

	BP_OnRiftMoveStarted(Marker);
	return true;
}

void ARiftNPC::PresentDialogue(const FString& Line)
{
	FacePlayer();
	BP_OnRiftDialogue(Line);
}
