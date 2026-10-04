// Copyright Epic Games, Inc. All Rights Reserved.

#include "RiftGameMode.h"
#include "RiftHUD.h"

ARiftGameMode::ARiftGameMode()
{
	// the Rift HUD shows what the backend decided: objective, banners, subtitles
	HUDClass = ARiftHUD::StaticClass();
}
