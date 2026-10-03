# game/ — Unreal Engine 5 project (manual setup required)

Status on this machine (2026-10-03): **not set up**.

| Requirement | Status |
|---|---|
| Xcode (full app) | MISSING — only Command Line Tools at `/Library/Developer/CommandLineTools` |
| Epic Games Launcher | MISSING |
| Unreal Engine 5 | MISSING |
| Hardware | Apple M3, 8 GB RAM (WARN: workable but slow), ~205 GB free disk (OK) |

These require GUI interaction, an Apple ID / Epic account and 50+ GB of
downloads, so they were not automated.

## 1. Install Xcode (~15–40 GB)

1. Open the Mac App Store, install **Xcode**. Before choosing a version, check the
   release notes of the Unreal Engine version you will install
   ("Platform SDK" / "macOS requirements") — UE pins a supported Xcode range.
   On macOS 15 the current App Store Xcode is normally what the latest UE 5.x wants.
2. Launch Xcode once and accept the license / install extra components.
3. Point the toolchain at it and accept the license:
   ```sh
   sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
   sudo xcodebuild -license accept
   xcodebuild -version        # should print Xcode 16.x or newer
   ```

## 2. Install Unreal Engine 5 (~40–60 GB)

1. Download the Epic Games Launcher: https://store.epicgames.com/download
2. Sign in → **Unreal Engine** tab → **Library** → **+** → choose the latest
   **5.x** release → Install (default location `/Users/Shared/Epic Games/UE_5.x`).
3. In the install **Options**, you can untick target platforms you don't need
   (Android, iOS, Linux) to save disk space.

## 3. Create the project

1. Launch Unreal Engine 5 from the Launcher.
2. **Games → First Person**.
3. Project Defaults: **C++** (not Blueprint), Target **Desktop**, Quality
   **Scalable** (recommended on 8 GB RAM), Starter Content **off**.
4. Project Location: `/Users/virajveerchowdhary/rift/game`
   Project Name: `Rift`
   → creates `game/Rift/Rift.uproject`.
5. Click **Create**. The editor compiles the C++ module (first build takes a while).

## 4. Enable WebSockets

Edit `game/Rift/Source/Rift/Rift.Build.cs` and add `"WebSockets"`:

```csharp
PublicDependencyModuleNames.AddRange(new string[] {
    "Core", "CoreUObject", "Engine", "InputCore", "EnhancedInput", "WebSockets"
});
```

(Keep whatever modules the template already lists; just append `"WebSockets"`.)
Then close the editor and rebuild: right-click `Rift.uproject` →
**Generate Xcode Project**, open `Rift (Mac).xcworkspace`, build the
`RiftEditor` scheme — or simply reopen the `.uproject` and accept the rebuild.

The WebSocket client itself is Phase 3 work — don't write it yet.

## 5. Verify (Phase 0 Unreal checklist)

- [ ] `game/Rift/Rift.uproject` opens in the editor
- [ ] C++ compiles (no "modules are missing or built with a different engine version" loop)
- [ ] Play In Editor: First Person template runs
- [ ] Player can move (WASD) and look (mouse)
- [ ] Project builds with `"WebSockets"` in `Rift.Build.cs`
- [ ] `make doctor` shows `Unreal Engine PASS` and `Unreal project PASS`

## 8 GB RAM tips

- Close browsers/other apps while the editor runs.
- **Settings → Engine Scalability Settings → Low** while developing.
- Project Settings: consider disabling Lumen (use Screen Space GI) and virtual
  shadow maps if the editor stutters.

## Rules for this folder

- No API keys, `.env` values or service URLs with credentials in the Unreal
  project — Unreal talks only to the local Rust backend.
- `Binaries/`, `Intermediate/`, `Saved/`, `DerivedDataCache/` are git-ignored.
