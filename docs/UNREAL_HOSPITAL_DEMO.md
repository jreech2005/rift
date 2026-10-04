# Unreal hospital demo — assembly checklist

How to build the hospital vertical slice in the Unreal Editor on top of the Phase 3 presentation
code (`game/Rift/Source/Rift/Presentation/`). The C++ is done; the level is assembled by hand.
Nothing here needs a Blueprint graph.

Status: the code compiles, the HUD logic and the voice helpers are covered by the automation
tests `Rift.Presentation.HudModel` and `Rift.Presentation.Voice`, and `Rift.NetSmoke` still passes. The level below has **not** been
assembled or looked at in the editor yet — the first person to do it should expect small fixes.

## How events reach the screen

```
Rust backend ──world_event──► URiftNetworkSubsystem      (socket + parsing, unchanged)
                                  │ OnWorldEvent
                                  ▼
                        URiftWorldPresentationSubsystem   (one per game world)
                          ├─ FRiftHudModel ─► ARiftHUD    (text on screen)
                          ├─ audio_url ─► HTTP GET ─► 2D sound (voiced dialogue, optional)
                          ├─ ARiftNPC                     (face player, walk to marker)
                          └─ URiftEntityComponent.OnRiftEvent (per actor, for Blueprints)
```

| `event_type` | What the player sees |
|---|---|
| `objective_updated` `active` | banner `NEW OBJECTIVE`, objective under `CURRENT OBJECTIVE` |
| `objective_updated` `failed` / `invalidated` | banner `OBJECTIVE FAILED`, objective turns red |
| `objective_updated` `completed` | banner `OBJECTIVE COMPLETE` |
| `mission_updated` `failed` / `invalidated` | banner `MISSION FAILED` |
| `mission_updated` `completed` / `active` | banner `MISSION COMPLETE` / `NEW MISSION: <title>` |
| `dialogue_started` | subtitle `<NPC name>: <opening_line>`, the NPC turns to the player; with an `audio_url` the line is also spoken |
| `information_revealed` to `player` | subtitle with the revealed text |
| `world_event_triggered` | description as a notice line; each NPC in `npc_ids` walks to a marker named after `event` |
| `npc_moved`, `npc_activated` | the NPC walks to the marker for `location_id` |
| `speech_acknowledged` | the NPC that was spoken to turns to the player |
| `world_flag_changed` | nothing on screen; `GetWorldFlag` / `OnWorldFlagChanged` for Blueprints |
| anything else | nothing; logged, forwarded to `OnAnyWorldEvent` |

Banners queue, each shows for 3.5 s.

## 1. Level

1. Open `game/Rift/Rift.uproject`. Use `Lvl_FirstPerson`, or **File → New Level → Basic** and save
   it as `Content/Hospital/Lvl_Hospital` (then set it as *Editor Startup Map* and *Game Default
   Map* in **Project Settings → Maps & Modes**).
2. Block out one room with a bed: cubes from `Content/LevelPrototyping/Meshes` are enough.
3. World Settings → *GameMode Override*: `BP_FirstPersonGameMode` (already the project default).
4. Make sure there is a **Player Start** inside the room.

## 2. NavMesh

1. **Place Actors → Volumes → Nav Mesh Bounds Volume**. Scale it to cover the whole floor,
   including the corridor Hank walks in from.
2. Press **P** in the viewport: the walkable floor must turn green. No green under an NPC or a
   marker means that NPC will not walk there (the log says `found no path to marker`).

## 3. NPCs — Hank and Walter

1. Content Browser → **Add → Blueprint Class** → search parent `RiftNPC` → name it `BP_RiftNPC`.
2. Open it, select **Mesh**: Skeletal Mesh `SKM_Manny_Simple`, Anim Class `ABP_Unarmed`,
   Location Z `-90`, Rotation Yaw `-90`. Compile, save.
3. Drag two `BP_RiftNPC` into the level. On each, select the **RiftEntity** component:

   | Actor | Rift Id | Display Name | Interact Action Type | Interact Content |
   |---|---|---|---|---|
   | Hank, in the corridor | `hank_schrader` | `Hank` | `speak` | `Walter has a burner phone hidden under his mattress.` |
   | Walter, by the bed | `walter_white` | `Walter` | `interact` | *(empty)* |

   With those settings, pressing **E** on Hank tells him the secret — the same action as
   `Rift.Speak hank_schrader Walter has a burner phone hidden under his mattress.`
   To keep Hank silent until the console is used, set his action to `interact`.
4. Leave *Auto Possess AI* on `Placed in World or Spawned` (the default of `RiftNPC`).

IDs are case sensitive and must match the backend exactly.

## 4. Props — mattress and burner phone

For each prop: place any Static Mesh actor (a flattened cube for the mattress, a small cube for
the phone), make sure it has collision, then **Details → Add → Rift Entity**:

| Actor | Rift Id | Display Name | Interact Action Type |
|---|---|---|---|
| mattress | `mattress` | `Mattress` | `interact` |
| burner phone | `burner_phone` | `Burner phone` | `inspect` |

`interact` on `mattress` sets the flag `interacted:mattress`, which completes the objective
`hide_burner_phone` in the burner phone scenario.

Any other actor (for example a `hospital_door`) joins the same way: add **Rift Entity**, set the id.

## 5. Markers

**Place Actors** → search `Rift Location Marker`. Put them on green NavMesh.

| Marker Id | Npc Id | Where | Used by |
|---|---|---|---|
| `confrontation_begins` | `hank_schrader` | beside the bed, facing Walter | `world_event_triggered` |
| `confrontation_begins` | `walter_white` | where Walter should stand, or omit so he stays | `world_event_triggered` |
| `albuquerque_hospital` | *(empty)* | room centre | `npc_moved` / `npc_activated` |

A marker with an *Npc Id* is reserved for that NPC; one without is used by any NPC that has no
reserved marker. The Director chooses the event; the marker and the NavMesh decide the path.

## 6. HUD

Nothing to bind. `ARiftGameMode` sets the HUD class to `RiftHUD`, and `BP_FirstPersonGameMode`
inherits it. If no text ever shows: open `BP_FirstPersonGameMode` → Class Defaults → **HUD Class**
→ set `RiftHUD`.

To replace it with UMG later: in a widget, **Get World Subsystem → Rift World Presentation
Subsystem**, bind `OnHudChanged` and read `GetHudState` (`CurrentObjective`, `Banner`, `Subtitle`,
`Notice`). Then set the HUD class back to `HUD`.

## 7. Voice (optional)

Nothing to place in the level: voiced lines play as 2D sound. It is all backend configuration, in
the repo-root `.env`:

```sh
ELEVENLABS_API_KEY=<key>
ELEVENLABS_VOICES=hank_schrader=<voice_id>,walter_white=<voice_id>
ELEVENLABS_OUTPUT_FORMAT=pcm_24000
ELEVENLABS_TIMEOUT_MS=3000
```

`ELEVENLABS_OUTPUT_FORMAT=pcm_24000` is required for sound in Unreal: the client plays 16-bit PCM
WAV and nothing else. With the backend default (MP3) the subtitle shows and the Output Log says
`Voice: clip is not 16 bit PCM WAV`.

When Hank speaks, the subtitle appears first and the Output Log shows
`LogRiftPresentation: Voice: playing 3.2 s at 24000 Hz` a moment later. Without a key, without a
voice for that NPC, or when the download fails, the subtitle is all there is — nothing else
changes. Check the editor is not muted (**Editor Preferences → Level Editor → Play → Enable Game
Sound**) and that the backend log said `voice: elevenlabs` at startup. Details: `docs/VOICE.md`.

## 8. Check it without the backend

Press Play, open the console (`` ` ``):

```
Rift.FakeEvent objective_updated hide_burner_phone objective_id=hide_burner_phone status=active title=Hide Walter's burner phone
Rift.FakeEvent objective_updated hide_burner_phone objective_id=hide_burner_phone status=failed title=Hide Walter's burner phone
Rift.FakeEvent mission_updated protect_walters_cover status=failed title=Protect Walter's cover
Rift.FakeEvent dialogue_started hank_schrader npc_id=hank_schrader opening_line=You did the right thing telling me.
Rift.FakeEvent world_event_triggered - event=confrontation_begins npc_ids=hank_schrader,walter_white description=Hank steps into the room.
Rift.FakeEvent npc_moved hank_schrader npc_id=hank_schrader location_id=albuquerque_hospital
```

`Rift.FakeEvent` only presents; nothing is sent to the backend. A payload can also be given as one
JSON object. (On a `-ExecCmds=` command line the comma in `npc_ids=a,b` splits the command; use
the in-game console for that one.)

## 9. Run it against the backend

Use the demo timings from `.env.example` (`DIRECTOR_BUDGET_MS=6000`, `GEMINI_TIMEOUT_MS=3000`) so
the Director answers, or the deterministic rules take over, within about six seconds.

```sh
cd <repo root>
RIFT_WORLD_BIBLE=backend/tests/fixtures/director/world_bible_breaking_bad.json \
RIFT_SCENARIO=backend/tests/fixtures/runtime/scenario_burner_phone.json \
make backend
```

Press Play. A session is created automatically once the backend answers
(`LogRiftNet: Rift session creation requested`, then `LogRiftNet: Rift session ready: <id>`).

Path A — keep the secret: look at the mattress, press **E** → `OBJECTIVE COMPLETE`.

Path B — give it away: look at Hank, press **E** (or
`Rift.Speak hank_schrader Walter has a burner phone hidden under his mattress.`) → Hank turns to
you, `OBJECTIVE FAILED`, then after the Director answers: notice line, Hank walks to his
`confrontation_begins` marker, Hank's subtitle, `NEW OBJECTIVE`.

Every Play session connects again and gets a new backend session, so each run starts the story
from the beginning.

## Troubleshooting

| Symptom | Check |
|---|---|
| no `[E] <name>` prompt | actor needs collision; the Rift Entity must be *Interactable*; stand within 3.5 m |
| E does nothing | Output Log `LogRiftNet`: no session → backend not running when Play started |
| NPC does not walk | NavMesh (press P); marker id spelled like the event; NPC has an AI controller |
| NPC slides without animation | Anim Class `ABP_Unarmed` on the mesh |
| no HUD text | section 6 |
| `Rift id '…' is used by both` | two actors share an id |
| subtitle but no voice | section 7: `ELEVENLABS_OUTPUT_FORMAT=pcm_24000`, a voice id for that NPC, `LogRiftPresentation: Voice:` lines |

## Limits

- Not yet seen running in the editor; E-key input, HUD layout and NPC walking are untested visually.
- The backend sends no objective at session start, so `CURRENT OBJECTIVE` is empty until the
  first `objective_updated`.
- One player, one level. NPCs are found by Rift id in the loaded level only.
- `npc_disposition_changed` has no built-in presentation; use `OnRiftEvent` on the NPC's entity.
- Voice playback has not been heard in the editor yet (no ElevenLabs credentials were available
  at integration time). It is 2D, without lip sync.
- No dialogue UI for the player's reply: speaking is a preset line or `Rift.Speak`.
