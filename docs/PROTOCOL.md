# Rift WebSocket Protocol — V1

Transport: one persistent WebSocket at `ws://<BACKEND_HOST>:<BACKEND_PORT>/ws`
(default `ws://127.0.0.1:3000/ws`). Text frames only, UTF-8 JSON, max 64 KiB per
message. Binary frames get a `malformed_message` error.

Source of truth: `backend/src/protocol.rs` and `backend/src/action.rs`.
JSON Schemas: `shared/schemas/v1/`.

## Envelope

Every message, in both directions:

```json
{
  "protocol_version": 1,
  "message_id": "2f0c6d0e-…",
  "message_type": "hello",
  "timestamp": "2026-10-03T12:00:00Z",
  "session_id": null,
  "payload": {}
}
```

| Field | Rules |
|---|---|
| `protocol_version` | Must be `1`. Anything else → `unsupported_protocol_version`. |
| `message_id` | 1–128 chars, chosen by the sender. UUIDs recommended. |
| `message_type` | See tables below. |
| `timestamp` | RFC 3339 / ISO 8601 date-time. |
| `session_id` | UUID or `null`. Required for `player_action`. |
| `payload` | Object; shape depends on `message_type`. Missing/`null` = `{}`. |
| `reply_to` | **Server only.** The `message_id` this message answers. Clients must not send it. |

Unknown envelope or payload fields are rejected (strict decoding).

## Handshake

The first message on a connection must be `hello` (`ping` is also allowed
before it). Anything else first → `handshake_required`.

## Client → Server

### `hello`
```json
{ "client": "rift-unreal", "client_version": "0.1.0" }
```
`client` required, `client_version` optional. → `hello_ack`

### `ping`
```json
{ "nonce": "abc" }
```
`nonce` optional. → `pong`

### `create_session`
Payload `{}`. Creates a real `GameSession` in Rust memory. → `session_created`

### `player_action`
Envelope `session_id` must be set and equal `payload.session_id`.
```json
{
  "protocol_version": 1,
  "action_id": "6f1c1b64-3f4b-4c1f-9a43-3f1a3d0a8b11",
  "session_id": "0b5e0f53-0a52-4c8f-8f0e-9d0a1b2c3d4e",
  "actor_id": "player",
  "action_type": "interact",
  "target": "test_door",
  "content": null,
  "timestamp": "2026-10-03T12:00:00Z"
}
```

| `action_type` | `target` | `content` | Resulting `event_type` | State change |
|---|---|---|---|---|
| `interact` | required | rejected | `interaction_acknowledged` | sets flag `interacted:<target>` |
| `inspect` | required | rejected | `inspection_acknowledged` | none |
| `move` | required | rejected | `location_changed` | sets `current_location` |
| `speak` | optional | required, ≤ 500 chars | `speech_acknowledged` | none |

Identifiers (`actor_id`, `target`): `^[A-Za-z0-9_.:-]{1,64}$`.
`action_id` is a client UUID; resubmitting an already-applied `action_id`
(within the last 32 events) → `duplicate_action`. → `world_event`

## Server → Client

### `hello_ack`
```json
{ "server": "rift-backend", "server_version": "0.1.0", "protocol_version": 1, "connection_id": "…" }
```

### `pong`
```json
{ "nonce": "abc" }
```

### `session_created`
Envelope `session_id` is the new session. Payload is the session snapshot:
```json
{
  "session_id": "…",
  "created_at": "…",
  "current_location": null,
  "world_flags": {},
  "recent_events": [],
  "event_count": 0
}
```

### `world_event`
```json
{
  "event_id": "…",
  "session_id": "…",
  "sequence": 1,
  "event_type": "interaction_acknowledged",
  "target": "test_door",
  "payload": { "actor_id": "player", "first_interaction": true },
  "timestamp": "…"
}
```
`event_id` is a UUIDv5 of `action_id`, so the same action always maps to the
same event. `sequence` increases by 1 per session.

### `error`
```json
{ "code": "session_not_found", "message": "session … does not exist" }
```
`reply_to` is set whenever the offending `message_id` could be read. Errors
never close the connection.

| Code | When |
|---|---|
| `malformed_message` | invalid JSON, not an object, bad envelope shape, bad timestamp, binary frame |
| `unsupported_protocol_version` | `protocol_version` ≠ 1 (envelope or action) |
| `unknown_message_type` | not a client message type |
| `invalid_payload` | payload does not match the message type |
| `handshake_required` | message other than `hello`/`ping` before `hello` |
| `session_not_found` | `player_action` for a session that does not exist |
| `session_mismatch` | envelope `session_id` missing or ≠ action `session_id` |
| `invalid_action` | unknown `action_type`, bad identifiers, missing/forbidden target/content |
| `duplicate_action` | `action_id` already applied |

## Versioning

V1 is additive-only: new message or event types may be added; existing fields
are not renamed or retyped. A breaking change means `protocol_version: 2`, and
the server rejects versions it does not speak.
