// Copyright Epic Games, Inc. All Rights Reserved.

using UnrealBuildTool;

public class Rift : ModuleRules
{
	public Rift(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;

		PublicDependencyModuleNames.AddRange(new string[] {
			"Core",
			"CoreUObject",
			"Engine",
			"InputCore",
			"EnhancedInput",
			"AIModule",
			"StateTreeModule",
			"GameplayStateTreeModule",
			"UMG",
			"Slate"
		});

		// Backend connection (Network/): persistent WebSocket + JSON protocol V1
		PrivateDependencyModuleNames.AddRange(new string[] {
			"WebSockets",
			"Json"
		});

		PublicIncludePaths.AddRange(new string[] {
			"Rift",
			"Rift/Network",
			"Rift/Presentation",
			"Rift/Variant_Horror",
			"Rift/Variant_Horror/UI",
			"Rift/Variant_Shooter",
			"Rift/Variant_Shooter/AI",
			"Rift/Variant_Shooter/UI",
			"Rift/Variant_Shooter/Weapons"
		});

		// Uncomment if you are using Slate UI
		// PrivateDependencyModuleNames.AddRange(new string[] { "Slate", "SlateCore" });

		// Uncomment if you are using online features
		// PrivateDependencyModuleNames.Add("OnlineSubsystem");

		// To include OnlineSubsystemSteam, add it to the plugins section in your uproject file with the Enabled attribute set to true
	}
}
