# /// script
# requires-python = ">=3.12"
# dependencies = ["websockets>=14"]
# ///
"""Rift protocol V1 smoke test.

Connects to a running backend and drives the full Phase 0 protocol path:
hello -> ping -> create_session -> player_action.

Usage:
    uv run scripts/smoke_ws.py [--url ws://127.0.0.1:3000/ws]
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import uuid
from datetime import UTC, datetime
from typing import Any

from websockets.asyncio.client import connect

PROTOCOL_VERSION = 1
TIMEOUT_S = 5.0


def now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def envelope(message_type: str, payload: dict[str, Any], session_id: str | None = None) -> dict:
    return {
        "protocol_version": PROTOCOL_VERSION,
        "message_id": str(uuid.uuid4()),
        "message_type": message_type,
        "timestamp": now(),
        "session_id": session_id,
        "payload": payload,
    }


class SmokeFailure(Exception):
    pass


def expect(cond: bool, what: str) -> None:
    if not cond:
        raise SmokeFailure(what)
    print(f"  ok  {what}")


async def request(ws, msg: dict) -> dict:
    await ws.send(json.dumps(msg))
    raw = await asyncio.wait_for(ws.recv(), TIMEOUT_S)
    reply = json.loads(raw)
    if reply.get("message_type") == "error":
        raise SmokeFailure(f"server error for {msg['message_type']}: {reply['payload']}")
    expect(reply.get("reply_to") == msg["message_id"], f"{reply['message_type']} correlates to request")
    expect(reply.get("protocol_version") == PROTOCOL_VERSION, "reply uses protocol v1")
    return reply


async def run(url: str) -> None:
    print(f"connecting to {url}")
    async with connect(url, open_timeout=TIMEOUT_S) as ws:
        ack = await request(ws, envelope("hello", {"client": "rift-smoke-cli", "client_version": "0.1.0"}))
        expect(ack["message_type"] == "hello_ack", "hello -> hello_ack")

        nonce = uuid.uuid4().hex
        pong = await request(ws, envelope("ping", {"nonce": nonce}))
        expect(pong["message_type"] == "pong" and pong["payload"].get("nonce") == nonce, "ping -> pong (nonce echoed)")

        created = await request(ws, envelope("create_session", {}))
        expect(created["message_type"] == "session_created", "create_session -> session_created")
        session_id = created["session_id"]
        expect(bool(session_id) and created["payload"]["session_id"] == session_id, f"session_id captured ({session_id})")

        action = {
            "protocol_version": PROTOCOL_VERSION,
            "action_id": str(uuid.uuid4()),
            "session_id": session_id,
            "actor_id": "player",
            "action_type": "interact",
            "target": "test_door",
            "timestamp": now(),
        }
        event = await request(ws, envelope("player_action", action, session_id))
        expect(event["message_type"] == "world_event", "player_action -> world_event")
        p = event["payload"]
        expect(p["event_type"] == "interaction_acknowledged", "event_type == interaction_acknowledged")
        expect(p["target"] == "test_door" and p["session_id"] == session_id, "event targets test_door in our session")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--url", default="ws://127.0.0.1:3000/ws")
    args = parser.parse_args()
    try:
        asyncio.run(run(args.url))
    except (SmokeFailure, OSError, TimeoutError, KeyError) as exc:
        print(f"Rift protocol smoke test: FAIL ({type(exc).__name__}: {exc})", file=sys.stderr)
        return 1
    print("Rift protocol smoke test: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
