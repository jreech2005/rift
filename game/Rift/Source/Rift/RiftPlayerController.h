// Copyright Epic Games, Inc. All Rights Reserved.

#pragma once

#include "CoreMinimal.h"
#include "GameFramework/PlayerController.h"
#include "RiftPlayerController.generated.h"

class UInputMappingContext;
class URiftEntityComponent;
class UUserWidget;

/**
 *  Simple first person Player Controller
 *  Manages the input mapping context.
 *  Overrides the Player Camera Manager class.
 */
UCLASS(abstract, config="Game")
class RIFT_API ARiftPlayerController : public APlayerController
{
	GENERATED_BODY()
	
public:

	/** Constructor */
	ARiftPlayerController();

	/** Returns the interactable Rift entity the player is looking at within reach, null when there is none */
	UFUNCTION(BlueprintCallable, Category="Rift")
	URiftEntityComponent* GetFocusedRiftEntity() const;

	/** Sends the focused entity's player action to the backend. Bound to the E key */
	UFUNCTION(BlueprintCallable, Category="Rift")
	void InteractWithFocused();

protected:

	/** How far the player can reach to interact, in cm */
	UPROPERTY(EditAnywhere, Category="Rift")
	float InteractDistance = 350.0f;

	/** Input Mapping Contexts */
	UPROPERTY(EditAnywhere, Category="Input|Input Mappings")
	TArray<UInputMappingContext*> DefaultMappingContexts;

	/** Input Mapping Contexts */
	UPROPERTY(EditAnywhere, Category="Input|Input Mappings")
	TArray<UInputMappingContext*> MobileExcludedMappingContexts;

	/** Mobile controls widget to spawn */
	UPROPERTY(EditAnywhere, Category="Input|Touch Controls")
	TSubclassOf<UUserWidget> MobileControlsWidgetClass;

	/** Pointer to the mobile controls widget */
	UPROPERTY()
	TObjectPtr<UUserWidget> MobileControlsWidget;

	/** If true, the player will use UMG touch controls even if not playing on mobile platforms */
	UPROPERTY(EditAnywhere, Config, Category = "Input|Touch Controls")
	bool bForceTouchControls = false;

	/** Gameplay initialization */
	virtual void BeginPlay() override;

	/** Input mapping context setup */
	virtual void SetupInputComponent() override;

	/** Returns true if the player should use UMG touch controls */
	bool ShouldUseTouchControls() const;
};
