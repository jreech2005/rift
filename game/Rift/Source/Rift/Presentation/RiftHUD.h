// Draws the Rift HUD state on the canvas: objective, banner, subtitle, interaction prompt.

#pragma once

#include "CoreMinimal.h"
#include "GameFramework/HUD.h"
#include "RiftHUD.generated.h"

/**
 *  Minimal player facing HUD. Reads URiftWorldPresentationSubsystem every frame and draws text, no assets needed.
 *  A UMG widget can replace it later by binding to the same subsystem.
 */
UCLASS()
class RIFT_API ARiftHUD : public AHUD
{
	GENERATED_BODY()

public:

	virtual void DrawHUD() override;

protected:

	/** Scales all HUD text */
	UPROPERTY(EditAnywhere, Category="Rift")
	float TextScale = 1.5f;

	/** Longest line of a subtitle or objective before it wraps, in characters */
	UPROPERTY(EditAnywhere, Category="Rift")
	int32 WrapColumns = 60;

private:

	/** Draws lines centered on X, starting at Y. Returns the Y below the last line */
	float DrawCentered(const TArray<FString>& Lines, const FLinearColor& Color, float CenterX, float Y, float Scale);

	/** Draws lines from X, starting at Y. Returns the Y below the last line */
	float DrawLeft(const TArray<FString>& Lines, const FLinearColor& Color, float X, float Y, float Scale);
};
