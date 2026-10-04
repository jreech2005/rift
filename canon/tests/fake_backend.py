"""A stand-in for the Rust backend, for the demo launcher tests.

Speaks just enough of the real surface: `GET /health` and the protocol V1
`hello` / `create_session` exchange over a WebSocket. Reads the same
environment the launcher gives the real backend. `FAKE_MODE` selects a failure:
`hang` never listens, `crash` logs a line (with a secret) and exits.
"""

import base64
import hashlib
import json
import os
import socketserver
import struct
import sys
import time
import uuid
from pathlib import Path

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def _read(sock, count: int) -> bytes:
    data = b""
    while len(data) < count:
        chunk = sock.recv(count - len(data))
        if not chunk:
            raise ConnectionError
        data += chunk
    return data


class Handler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        head = b""
        while b"\r\n\r\n" not in head:
            head += _read(self.request, 1)
        lines = head.decode().split("\r\n")
        path = lines[0].split()[1]
        headers = {k.lower(): v.strip() for k, _, v in (line.partition(":") for line in lines[1:])}
        if path == "/health":
            body = json.dumps({"status": "ok", "service": "rift-backend", "protocol_version": 1})
            self.request.sendall(
                f"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n{body}".encode()
            )
            return
        digest = hashlib.sha1((headers["sec-websocket-key"] + WS_GUID).encode()).digest()  # noqa: S324
        self.request.sendall(
            (
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Accept: {base64.b64encode(digest).decode()}\r\n\r\n"
            ).encode()
        )
        try:
            while True:
                self.reply(json.loads(self.frame()))
        except (ConnectionError, ValueError):
            return

    def frame(self) -> str:
        first, second = _read(self.request, 2)
        if first & 0x0F == 8:
            raise ConnectionError
        size = second & 0x7F
        if size == 126:
            (size,) = struct.unpack(">H", _read(self.request, 2))
        mask = _read(self.request, 4)
        return bytes(b ^ mask[i % 4] for i, b in enumerate(_read(self.request, size))).decode()

    def reply(self, message: dict) -> None:
        session_id = None
        if message["message_type"] == "hello":
            kind, payload = "hello_ack", {}
        else:
            session_id = str(uuid.uuid4())
            scenario = json.loads(Path(os.environ["RIFT_SCENARIO"]).read_text(encoding="utf-8"))
            location = scenario["world"]["player_location"]
            kind, payload = (
                "session_created",
                {"session_id": session_id, "current_location": location},
            )
            print(f"INFO story started session_id={session_id}", flush=True)
        data = json.dumps(
            {
                "protocol_version": 1,
                "message_id": str(uuid.uuid4()),
                "message_type": kind,
                "reply_to": message["message_id"],
                "session_id": session_id,
                "payload": payload,
            }
        ).encode()
        size = (
            struct.pack(">BB", 0x81, len(data))
            if len(data) < 126
            else struct.pack(">BBH", 0x81, 126, len(data))
        )
        self.request.sendall(size + data)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main() -> int:
    mode = os.environ.get("FAKE_MODE", "ok")
    if mode == "crash":
        print(f"ERROR cannot start, key={os.environ.get('GEMINI_API_KEY')}", flush=True)
        return 3
    if mode == "hang":
        print("INFO starting very slowly", flush=True)
        time.sleep(600)
    bible = json.loads(Path(os.environ["RIFT_WORLD_BIBLE"]).read_text(encoding="utf-8"))
    print(f"INFO world loaded universe_id={bible['universe']['universe_id']}", flush=True)
    with Server(("127.0.0.1", int(os.environ["BACKEND_PORT"])), Handler) as server:
        server.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
