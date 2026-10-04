# NPC Voice (Phase 3)

When the Director opens a dialogue, the backend can speak the line with ElevenLabs. Voice is
optional and best-effort: the line always arrives as text, and gains an `audio_url` only when
synthesis succeeded.

Code: `backend/src/voice/` (`mod.rs`, `elevenlabs.rs`, `cache.rs`), wired in
`backend/src/runtime/mod.rs` (`Runtime::complete`) and `backend/src/lib.rs` (`GET /audio/{id}`).

## Flow

```text
Director start_dialogue
   └─► Runtime::apply_decision        validated, applied, recorded (sync, as before)
         └─► dialogue_started event    { npc_id, opening_line, text }
               └─► VoiceService.voice_line(npc_id, text)       async, bounded by a timeout
                     ├─ voice id for the NPC?  no ──► text only, no API call
                     └─► VoiceProvider (ElevenLabsVoiceProvider)
                           POST /v1/text-to-speech/{voice_id}   body: { text, model_id }
                           ├─ ok ──► AudioCache.insert ──► payload.audio_url = "/audio/<uuid>"
                           └─ error / timeout / bad audio ──► text only
   └─► world_event pushed over the WebSocket
         └─► Unreal: show `text`; if `audio_url` is present, GET it and play it
```

- Synthesis runs in `Runtime::complete`, the asynchronous half of an action that already waits
  for the Director. The player-action acknowledgement and the narrative consequences are sent
  before it starts. No gameplay state depends on it.
- Only `dialogue_started` events reach the voice provider. A decision without `start_dialogue`
  makes no call, and neither does a line from an NPC that has no configured voice.
- Voice decorates the outgoing event only. The session's recorded state, the sequence numbers and
  the Director's report are identical with and without voice.

## Provider interface

```rust
pub trait VoiceProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn synthesize<'a>(&'a self, request: VoiceRequest<'a>)   // npc_id, text, voice_id
        -> BoxFuture<'a, Result<VoiceAudio, VoiceError>>;     // bytes + content type
}
```

| Provider | Use |
|---|---|
| `ElevenLabsVoiceProvider` | production |
| `NoopVoiceProvider` | always `not_configured` |
| `ScriptedVoiceProvider` | tests: canned audio, errors or a hang; records its calls |

`VoiceService` owns the provider, the NPC → voice map, the cache and the timeout.
`VoiceService::voice_line` returns `Option<String>` and never an error.

## Configuration

All optional. Without `ELEVENLABS_API_KEY` or without any voice, the backend logs
`voice: disabled, dialogue is text only` and runs as before.

| Variable | Default | Notes |
|---|---|---|
| `ELEVENLABS_API_KEY` | — | server-side only; sent as the `xi-api-key` header, never logged |
| `ELEVENLABS_VOICES` | — | `npc_id=voice_id` pairs, comma separated, e.g. `hank_schrader=<id>,walter_white=<id>` |
| `ELEVENLABS_MODEL` | `eleven_flash_v2_5` | low-latency model |
| `ELEVENLABS_OUTPUT_FORMAT` | `mp3_44100_128` | any ElevenLabs `output_format` |
| `ELEVENLABS_TIMEOUT_MS` | `6000` | per line; after it the line goes out as text |

NPC ids are WorldBible character ids. Voice ids must be ASCII letters and digits. Put them in the
git-ignored `.env`; `.env.example` carries placeholders only.

## Delivery

`audio_url` is a path on the backend's HTTP origin (the host and port that serve `/ws`), e.g.
`/audio/3f0c…`. The audio itself never travels over the WebSocket.

`GET /audio/{id}` returns the clip with its `Content-Type` (`audio/mpeg` by default) and
`Cache-Control: no-store`, or `404` when the id is unknown, expired or evicted. Ids are random
UUIDv4.

The cache is in memory and bounded:

| Limit | Value |
|---|---|
| clips | 32 (oldest evicted first) |
| total size | 16 MB |
| age | 10 minutes |
| one clip | 2 MB (larger responses are rejected) |

A restart drops every clip. Clients should fetch a clip when the event arrives.

## Failure behaviour

Dialogue is never suppressed because of voice. In every case below the `dialogue_started` event is
delivered with `text` and without `audio_url`, the rest of the decision's events follow, and the
session carries on. The client falls back to a subtitle.

| Situation | Result |
|---|---|
| no API key / no voices configured | voice disabled at startup |
| NPC has no voice id | no API call |
| ElevenLabs unreachable, 5xx, 429, 401/403, other non-2xx | warning logged with the error kind and HTTP status |
| no answer within the timeout | warning logged |
| `200` that is not audio, is empty or exceeds 2 MB | treated as malformed |
| clip expired or evicted before the client fetched it | `GET` returns `404` |

## Privacy and secrets

- The request body is `{ "text": <the line to speak>, "model_id": <model> }`. No session id, player
  input, NPC memory, world flag or Director context is sent.
- The API key lives in `director::Secret` (redacted `Debug`, no `Display`/`Serialize`) and is
  marked sensitive on the request. Errors carry a category and a status code, never the request
  URL, headers or the provider's response text.
- Nothing about ElevenLabs reaches the game client except the `audio_url` path.

## Tests

`cargo test` never contacts ElevenLabs. `backend/tests/voice.rs` runs the provider against a local
HTTP stand-in (success, 5xx/429/401, unreachable, timeout, malformed responses) and the runtime
over a real WebSocket with `ScriptedVoiceProvider` (audio URL and retrieval, text-only fallback on
error and on timeout, no call without a dialogue action or without a voice). Cache bounds are unit
tested in `backend/src/voice/cache.rs`.

Live check (spends a few characters of quota; fails with `BLOCKED` without credentials):

```sh
make voice-live
```

## Limits

- The events of a decision that contains a dialogue are held until synthesis ends or times out.
- One clip per line, synthesized in full before it is offered: no streaming, no lip sync, no voice
  cloning, no on-disk cache.
- The default format is MP3. A client that needs PCM can set `ELEVENLABS_OUTPUT_FORMAT`.
- The Unreal client does not fetch or play `audio_url` yet; it ignores the new fields.
