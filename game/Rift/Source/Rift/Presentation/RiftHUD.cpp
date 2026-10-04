// Draws the Rift HUD state on the canvas: objective, banner, subtitle, interaction prompt.

#include "RiftHUD.h"

#include "Engine/Canvas.h"
#include "Engine/Engine.h"
#include "RiftEntityComponent.h"
#include "RiftPlayerController.h"
#include "RiftWorldPresentationSubsystem.h"

namespace
{
	const FLinearColor LabelColor(0.75f, 0.75f, 0.75f);
	const FLinearColor FailedColor(1.0f, 0.2f, 0.15f);
	const FLinearColor BannerColor(1.0f, 0.85f, 0.2f);

	/** Breaks text into lines of at most Columns characters, at spaces */
	TArray<FString> Wrap(const FString& Text, int32 Columns)
	{
		TArray<FString> Words;
		Text.ParseIntoArrayWS(Words);

		TArray<FString> Lines;
		FString Line;

		for (const FString& Word : Words)
		{
			if (!Line.IsEmpty() && Line.Len() + 1 + Word.Len() > Columns)
			{
				Lines.Add(Line);
				Line.Reset();
			}

			if (!Line.IsEmpty())
			{
				Line += TEXT(" ");
			}

			Line += Word;
		}

		if (!Line.IsEmpty())
		{
			Lines.Add(Line);
		}

		return Lines;
	}
}

void ARiftHUD::DrawHUD()
{
	Super::DrawHUD();

	const URiftWorldPresentationSubsystem* Presentation = GetWorld()->GetSubsystem<URiftWorldPresentationSubsystem>();

	if (!Canvas || !Presentation)
	{
		return;
	}

	const FRiftHudState State = Presentation->GetHudState();

	const float Width = Canvas->ClipX;
	const float Height = Canvas->ClipY;
	const float Margin = 40.0f;

	if (State.bHasObjective)
	{
		const bool bFailed = State.bObjectiveFailed;

		const FString Label = bFailed ? TEXT("OBJECTIVE FAILED") : TEXT("CURRENT OBJECTIVE");

		const float Y = DrawLeft({ Label }, bFailed ? FailedColor : LabelColor, Margin, Margin, TextScale * 0.8f);
		DrawLeft(Wrap(State.CurrentObjective.Title, WrapColumns), bFailed ? FailedColor : FLinearColor::White, Margin, Y, TextScale);
	}

	if (!State.Banner.IsEmpty())
	{
		const bool bBad = State.Banner.Contains(TEXT("FAILED"));
		DrawCentered({ State.Banner }, bBad ? FailedColor : BannerColor, Width * 0.5f, Height * 0.22f, TextScale * 2.0f);
	}

	if (!State.Notice.IsEmpty())
	{
		DrawCentered(Wrap(State.Notice, WrapColumns), LabelColor, Width * 0.5f, Height * 0.32f, TextScale);
	}

	if (!State.Subtitle.IsEmpty())
	{
		const FString Speaker = Presentation->GetDisplayName(State.SubtitleSpeakerId);
		const FString Text = Speaker.IsEmpty() ? State.Subtitle : FString::Printf(TEXT("%s: %s"), *Speaker, *State.Subtitle);

		DrawCentered(Wrap(Text, WrapColumns), FLinearColor::White, Width * 0.5f, Height * 0.78f, TextScale);
	}

	if (const ARiftPlayerController* Player = Cast<ARiftPlayerController>(PlayerOwner))
	{
		if (const URiftEntityComponent* Focused = Player->GetFocusedRiftEntity())
		{
			const FString Prompt = FString::Printf(TEXT("[E] %s"), *Focused->GetDisplayNameOrId());
			DrawCentered({ Prompt }, FLinearColor::White, Width * 0.5f, Height * 0.56f, TextScale);
		}
	}
}

float ARiftHUD::DrawCentered(const TArray<FString>& Lines, const FLinearColor& Color, float CenterX, float Y, float Scale)
{
	UFont* Font = GEngine->GetMediumFont();

	for (const FString& Line : Lines)
	{
		float LineWidth = 0.0f;
		float LineHeight = 0.0f;
		GetTextSize(Line, LineWidth, LineHeight, Font, Scale);

		DrawText(Line, Color, CenterX - LineWidth * 0.5f, Y, Font, Scale);
		Y += LineHeight + 2.0f;
	}

	return Y;
}

float ARiftHUD::DrawLeft(const TArray<FString>& Lines, const FLinearColor& Color, float X, float Y, float Scale)
{
	UFont* Font = GEngine->GetMediumFont();

	for (const FString& Line : Lines)
	{
		float LineWidth = 0.0f;
		float LineHeight = 0.0f;
		GetTextSize(Line, LineWidth, LineHeight, Font, Scale);

		DrawText(Line, Color, X, Y, Font, Scale);
		Y += LineHeight + 2.0f;
	}

	return Y;
}
