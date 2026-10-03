#!/usr/bin/env bash
# Build and start the backend, run the WebSocket smoke client, then stop it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
HOST="${BACKEND_HOST:-127.0.0.1}"
PORT="${BACKEND_PORT:-3000}"
LOG="$(mktemp -t rift-backend.XXXXXX)"

if curl -fsS "http://$HOST:$PORT/health" >/dev/null 2>&1; then
  echo "error: something is already listening on $HOST:$PORT; stop it first" >&2
  exit 1
fi

cargo build --quiet --manifest-path "$ROOT/backend/Cargo.toml"
"$ROOT/backend/target/debug/rift-backend" >"$LOG" 2>&1 &
PID=$!
cleanup() { kill "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; rm -f "$LOG"; }
trap cleanup EXIT

for _ in $(seq 1 50); do
  if curl -fsS "http://$HOST:$PORT/health" >/dev/null 2>&1; then break; fi
  if ! kill -0 "$PID" 2>/dev/null; then echo "backend exited early:" >&2; cat "$LOG" >&2; exit 1; fi
  sleep 0.1
done

echo "health: $(curl -fsS "http://$HOST:$PORT/health")"
uv run --quiet "$ROOT/scripts/smoke_ws.py" --url "ws://$HOST:$PORT/ws"
