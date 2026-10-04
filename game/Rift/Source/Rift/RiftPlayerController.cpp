// Copyright Epic Games, Inc. All Rights Reserved.


#include "RiftPlayerController.h"
#include "EnhancedInputSubsystems.h"
#include "Engine/LocalPlayer.h"
#include "InputMappingContext.h"
#include "RiftCameraManager.h"
#include "Blueprint/UserWidget.h"
#include "Rift.h"
#include "RiftEntityComponent.h"
#include "Engine/World.h"
#include "InputCoreTypes.h"
#include "Widgets/Input/SVirtualJoystick.h"

ARiftPlayerController::ARiftPlayerController()
{
	// set the player camera manager class
	PlayerCameraManagerClass = ARiftCameraManager::StaticClass();
}

void ARiftPlayerController::BeginPlay()
{
	Super::BeginPlay();

	
	// only spawn touch controls on local player controllers
	if (IsLocalPlayerController() && ShouldUseTouchControls())
	{
		// spawn the mobile controls widget
		MobileControlsWidget = CreateWidget<UUserWidget>(this, MobileControlsWidgetClass);

		if (MobileControlsWidget)
		{
			// add the controls to the player screen
			MobileControlsWidget->AddToPlayerScreen(0);

		} else {

			UE_LOG(LogRift, Error, TEXT("Could not spawn mobile controls widget."));

		}

	}
}

void ARiftPlayerController::SetupInputComponent()
{
	Super::SetupInputComponent();

	// only add IMCs for local player controllers
	if (IsLocalPlayerController())
	{
		// Add Input Mapping Context
		if (UEnhancedInputLocalPlayerSubsystem* Subsystem = ULocalPlayer::GetSubsystem<UEnhancedInputLocalPlayerSubsystem>(GetLocalPlayer()))
		{
			for (UInputMappingContext* CurrentContext : DefaultMappingContexts)
			{
				Subsystem->AddMappingContext(CurrentContext, 0);
			}

			// only add these IMCs if we're not using mobile touch input
			if (!ShouldUseTouchControls())
			{
				for (UInputMappingContext* CurrentContext : MobileExcludedMappingContexts)
				{
					Subsystem->AddMappingContext(CurrentContext, 0);
				}
			}
		}

		// a plain key binding: interacting needs no input asset
		InputComponent->BindKey(EKeys::E, IE_Pressed, this, &ARiftPlayerController::InteractWithFocused);
	}
	
}

URiftEntityComponent* ARiftPlayerController::GetFocusedRiftEntity() const
{
	FVector ViewLocation;
	FRotator ViewRotation;
	GetPlayerViewPoint(ViewLocation, ViewRotation);

	FCollisionQueryParams Params(SCENE_QUERY_STAT(RiftInteract), false, GetPawn());

	// by object type: character capsules ignore the visibility channel
	FCollisionObjectQueryParams ObjectTypes;
	ObjectTypes.AddObjectTypesToQuery(ECC_Pawn);
	ObjectTypes.AddObjectTypesToQuery(ECC_WorldStatic);
	ObjectTypes.AddObjectTypesToQuery(ECC_WorldDynamic);
	ObjectTypes.AddObjectTypesToQuery(ECC_PhysicsBody);

	FHitResult Hit;
	const FVector End = ViewLocation + ViewRotation.Vector() * InteractDistance;

	// a thin sphere is easier to aim than a line
	if (!GetWorld()->SweepSingleByObjectType(Hit, ViewLocation, End, FQuat::Identity, ObjectTypes, FCollisionShape::MakeSphere(12.0f), Params))
	{
		return nullptr;
	}

	const AActor* HitActor = Hit.GetActor();
	URiftEntityComponent* Entity = HitActor ? HitActor->FindComponentByClass<URiftEntityComponent>() : nullptr;

	return Entity && Entity->bInteractable ? Entity : nullptr;
}

void ARiftPlayerController::InteractWithFocused()
{
	if (URiftEntityComponent* Entity = GetFocusedRiftEntity())
	{
		Entity->Interact();
	}
}

bool ARiftPlayerController::ShouldUseTouchControls() const
{
	// are we on a mobile platform? Should we force touch?
	return SVirtualJoystick::ShouldDisplayTouchInterface() || bForceTouchControls;
}
