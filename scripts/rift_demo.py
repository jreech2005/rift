#!/usr/bin/env python3
"""Rift demo launcher: one command from a title to a ready backend and Unreal.

    python3 scripts/rift_demo.py start --title "The Matrix" [--no-unreal] [--new]
    python3 scripts/rift_demo.py stop | status | logs | preflight

Normal mode uses only files that are already on disk (cached or checked-in
WorldBibles and scenarios) and makes no network request. `--new` compiles a
title first and may call TMDB, Wikipedia and Gemini. See docs/DEMO.md.

Standard library only, so it runs with the system python3. Never prints secret
values: configuration is reported as PRESENT/MISSING.
"""

from __future__ import annotations

import argparse
import base64
import contextlib
import json
import os
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time
import urllib.request
import uuid
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_HOME = Path.home() / "rift-phase3-integration"
BACKEND_NAME = "rift-backend"
PROTOCOL_VERSION = 1

# Universes whose demo files are hand-picked rather than discovered. Breaking
# Bad is the hospital demo the Unreal level is built for
# (docs/UNREAL_HOSPITAL_DEMO.md): an authored scenario with a secret.
PINNED = [
    (
        "backend/tests/fixtures/director/world_bible_breaking_bad.json",
        "backend/tests/fixtures/runtime/scenario_burner_phone.json",
    ),
]
# Checked-in WorldBibles and generated scenarios: used when the cache is empty.
FIXTURE_DIRS = ["backend/tests/fixtures/scenario_builder"]

REPORTED_KEYS = [
    "GEMINI_API_KEY",
    "ELEVENLABS_API_KEY",
    "ELEVENLABS_VOICES",
    "TIGER_DATABASE_URL",
    "TIDB_HOST",
    "TMDB_API_KEY",
]
_SECRET_KEY = re.compile(r"KEY|TOKEN|SECRET|PASSWORD|PASSWD|_URL$|VOICES")
_secrets: set[str] = set()


class DemoError(Exception):
    """A failure with a message that is safe and short enough to show a judge."""


def say(text: str = "") -> None:
    print(redact(text), flush=True)


def redact(text: str) -> str:
    for secret in _secrets:
        text = text.replace(secret, "***")
    return text


# --- configuration ----------------------------------------------------------


@dataclass
class Config:
    root: Path
    home: Path
    demo_dir: Path
    env: dict[str, str]
    env_file: Path | None
    host: str
    port: int

    @property
    def log(self) -> Path:
        return self.demo_dir / "rift-backend.log"

    @property
    def state_file(self) -> Path:
        return self.demo_dir / "state.json"

    @property
    def cache_dir(self) -> Path:
        return self.root / "cache" / "universes"

    @property
    def search_dirs(self) -> list[Path]:
        """Where WorldBibles and scenarios are looked up, best first."""
        override = self.env.get("RIFT_DEMO_CACHE", "").strip()
        if override:
            dirs = [Path(item).expanduser() for item in override.split(os.pathsep) if item]
        else:
            dirs = [self.cache_dir, self.home / "cache" / "universes"]
        dirs += [self.root / item for item in FIXTURE_DIRS]
        unique: list[Path] = []
        for item in dirs:
            if item.is_dir() and item.resolve() not in [u.resolve() for u in unique]:
                unique.append(item)
        return unique

    @property
    def uproject(self) -> Path:
        override = self.env.get("RIFT_UPROJECT", "").strip()
        if override:
            return Path(override).expanduser()
        return self.home / "game" / "Rift" / "Rift.uproject"

    @property
    def probe_host(self) -> str:
        return "127.0.0.1" if self.host in ("0.0.0.0", "::") else self.host  # noqa: S104


def parse_env_file(path: Path) -> dict[str, str]:
    """KEY=VALUE lines. Enough of the dotenv format for the project's .env."""
    values: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.removeprefix("export ").partition("=")
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        else:
            value = value.split(" #", 1)[0].strip()
        values[key.strip()] = value
    return values


def load_config(root: Path = ROOT, environ: dict[str, str] | None = None) -> Config:
    """The process environment wins over the .env file, like dotenv does."""
    base = dict(os.environ if environ is None else environ)
    home = Path(base.get("RIFT_DEMO_HOME") or DEFAULT_HOME).expanduser()
    explicit = base.get("RIFT_ENV_FILE", "").strip()
    candidates = [Path(explicit).expanduser()] if explicit else [root / ".env", home / ".env"]
    env_file = next((item for item in candidates if item.is_file()), None)
    if explicit and env_file is None:
        raise DemoError(f"RIFT_ENV_FILE does not exist: {explicit}")
    env = {**(parse_env_file(env_file) if env_file else {}), **base}
    for key, value in env.items():
        if _SECRET_KEY.search(key) and len(value) >= 6:
            _secrets.add(value)
    try:
        port = int(env.get("BACKEND_PORT", "").strip() or 3000)
    except ValueError:
        raise DemoError("BACKEND_PORT is not a valid port number") from None
    return Config(
        root=root,
        home=home,
        demo_dir=root / ".demo",
        env=env,
        env_file=env_file,
        host=env.get("BACKEND_HOST", "").strip() or "127.0.0.1",
        port=port,
    )


def presence(config: Config, key: str) -> str:
    return "PRESENT" if config.env.get(key, "").strip() else "MISSING"


# --- title and scenario resolution -------------------------------------------


@dataclass
class Universe:
    title: str
    universe_id: str
    bible_path: Path
    bible: dict[str, Any]
    scenario_path: Path | None = None  # set only for pinned demos


def _tokens(text: str) -> list[str]:
    text = re.sub(r"['’]", "", text.casefold())
    tokens = re.findall(r"[a-z0-9]+", text)
    return tokens[1:] if len(tokens) > 1 and tokens[0] == "the" else tokens


def _load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def _as_universe(path: Path) -> Universe | None:
    """`path` as a WorldBible, recognised by its content and not by its name."""
    data = _load_json(path)
    if not isinstance(data, dict) or "opening_conflict" not in data:
        return None
    universe = data.get("universe")
    if not isinstance(universe, dict):
        return None
    title, universe_id = universe.get("title"), universe.get("universe_id")
    if not isinstance(title, str) or not isinstance(universe_id, str):
        return None
    return Universe(title=title, universe_id=universe_id, bible_path=path, bible=data)


def _json_files(directory: Path) -> list[Path]:
    return sorted(p for p in directory.glob("*.json") if not p.name.startswith("."))


def available_universes(config: Config) -> list[Universe]:
    """Every WorldBible on disk, best candidate first."""
    found: list[Universe] = []
    for bible, scenario in PINNED:
        universe = _as_universe(config.root / bible)
        if universe is not None and (config.root / scenario).is_file():
            universe.scenario_path = config.root / scenario
            found.append(universe)
    for directory in config.search_dirs:
        found += [u for u in map(_as_universe, _json_files(directory)) if u is not None]
    return found


def _match(query: list[str], universe: Universe) -> int:
    title = _tokens(universe.title)
    if query in (title, _tokens(universe.universe_id.replace("_", " "))):
        return 3
    if title[: len(query)] == query:
        return 2
    return 1 if all(token in title for token in query) else 0


def resolve_title(config: Config, title: str, exact: bool = False) -> Universe:
    """The cached WorldBible for `title`, matched on its universe metadata.

    Exact title first, then a title that starts with the words given
    ("Harry Potter"), then one that contains all of them.
    """
    query = _tokens(title)
    if not query:
        raise DemoError('a title is required, e.g. make demo TITLE="The Matrix"')
    universes = available_universes(config)
    scored = [(_match(query, universe), universe) for universe in universes]
    best = max((score for score, _ in scored), default=0)
    if best < (3 if exact else 1):
        known = sorted({u.title for u in universes})
        raise DemoError(
            f'no compiled universe matches "{title}".\n'
            f"Available: {', '.join(known) or 'none'}\n"
            f'Compile it first with: make demo-new TITLE="{title}"'
        )
    matches = [universe for score, universe in scored if score == best]
    titles = sorted({u.title for u in matches})
    if len({u.universe_id for u in matches}) > 1:
        raise DemoError(f'"{title}" is ambiguous: {", ".join(titles)}. Use the full title.')
    # Prefer a WorldBible that already has a scenario: nothing to generate.
    for universe in matches:
        if universe.scenario_path or find_scenario(config, universe):
            return universe
    return matches[0]


def _stem(path: Path) -> str:
    for suffix in (".world.json", ".scenario.json", ".json"):
        if path.name.endswith(suffix):
            return path.name[: -len(suffix)]
    return path.stem


def _fits(scenario: Any, universe: Universe) -> bool:
    """A scenario for this universe that names only what this WorldBible has."""
    if not isinstance(scenario, dict) or not isinstance(scenario.get("plan"), dict):
        return False
    if scenario["plan"].get("universe_id") != universe.universe_id:
        return False
    world = scenario.get("world") or {}
    ids = {
        kind: {item.get("id") for item in universe.bible.get(kind, [])}
        for kind in ("characters", "locations")
    }
    return set(world.get("characters", {})) <= ids["characters"] and (
        set(world.get("locations", [])) <= ids["locations"]
    )


def find_scenario(config: Config, universe: Universe) -> Path | None:
    """An existing scenario for `universe`: its own pair first, then any that fits."""
    if universe.scenario_path:
        return universe.scenario_path
    bible = universe.bible_path
    candidates = [bible.with_name(f"{_stem(bible)}.scenario.json")]
    for directory in [bible.parent, *config.search_dirs]:
        candidates += [p for p in _json_files(directory) if p.name.endswith(".scenario.json")]
    for path in candidates:
        if path.is_file() and _fits(_load_json(path), universe):
            return path
    return None


def uv_command() -> list[str]:
    uv = shutil.which("uv")
    if uv is None:
        raise DemoError("uv is not installed (needed to build a scenario): see docs/SETUP.md")
    return [uv, "run", "--quiet", "python"]


def build_scenario(config: Config, universe: Universe) -> Path:
    """Run the deterministic, offline Scenario Builder (no model, no network)."""
    bible = universe.bible_path
    # Beside the WorldBible, unless that is a checked-in fixture directory.
    tracked = (config.root / "backend").resolve() in bible.resolve().parents
    target_dir = config.cache_dir if tracked else bible.parent
    target = target_dir / f"{_stem(bible)}.scenario.json"
    command = [*uv_command(), "-m", "rift_canon.scenario", "--world", str(bible)]
    result = subprocess.run(  # noqa: S603
        [*command, "--output", str(target)],
        cwd=config.root / "canon",
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    if result.returncode != 0 or not target.is_file():
        detail = "\n".join((result.stderr or result.stdout).strip().splitlines()[-8:])
        raise DemoError(f"the Scenario Builder failed for {bible.name}:\n{detail}")
    return target


def compile_title(config: Config, title: str) -> Universe:
    """New-title mode: TMDB + Wikipedia + Gemini. Slow, needs keys and a network."""
    slug = "_".join(_tokens(title)) or "universe"
    target = config.cache_dir / f"{slug}.world.json"
    say(f'Compiling "{title}" (TMDB, Wikipedia, Gemini). This can take a few minutes...')
    command = [*uv_command(), "-m", "rift_canon.compile", title, "--output", str(target)]
    result = subprocess.run(command, cwd=config.root / "canon", env=config.env, check=False)  # noqa: S603
    universe = _as_universe(target) if result.returncode == 0 else None
    if universe is None:
        raise DemoError(f'could not compile "{title}" (see the messages above)')
    return universe


# --- processes ----------------------------------------------------------------


@dataclass
class Proc:
    pid: int
    started: str
    command: str

    @property
    def is_rift(self) -> bool:
        return Path(self.command).name == BACKEND_NAME


def proc_info(pid: int) -> Proc | None:
    """A live process, or None. `started` tells a reused PID from the original."""
    result = subprocess.run(  # noqa: S603
        ["ps", "-p", str(pid), "-o", "stat=,lstart=,comm="],  # noqa: S607
        capture_output=True,
        text=True,
        check=False,
    )
    fields = result.stdout.strip().split(None, 6)
    if result.returncode != 0 or len(fields) < 7 or fields[0].startswith("Z"):
        return None
    return Proc(pid=pid, started=" ".join(fields[1:6]), command=fields[6])


def listeners(port: int) -> list[int]:
    lsof = shutil.which("lsof") or "/usr/sbin/lsof"
    try:
        result = subprocess.run(  # noqa: S603
            [lsof, "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-t"],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return []
    return sorted({int(line) for line in result.stdout.split() if line.isdigit()})


def port_open(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=1):
            return True
    except OSError:
        return False


def stop_pid(pid: int, grace: float = 5.0) -> None:
    """SIGTERM, then SIGKILL if the process is still there after `grace`."""
    for sig, wait in ((signal.SIGTERM, grace), (signal.SIGKILL, 2.0)):
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            return
        deadline = time.monotonic() + wait
        while time.monotonic() < deadline:
            with contextlib.suppress(ChildProcessError):
                os.waitpid(pid, os.WNOHANG)  # reap it when it is our own child
            if proc_info(pid) is None:
                return
            time.sleep(0.1)


def read_state(config: Config) -> dict[str, Any]:
    state = _load_json(config.state_file)
    return state if isinstance(state, dict) else {}


def write_state(config: Config, state: dict[str, Any]) -> None:
    config.demo_dir.mkdir(parents=True, exist_ok=True)
    temporary = config.state_file.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    os.replace(temporary, config.state_file)


def managed(config: Config) -> Proc | None:
    """The backend this launcher started, if that exact process is still alive."""
    state = read_state(config)
    pid = state.get("pid")
    if not isinstance(pid, int):
        return None
    proc = proc_info(pid)
    return proc if proc is not None and proc.started == state.get("started") else None


def clear_pid(config: Config) -> None:
    state = read_state(config)
    if "pid" in state:
        state.pop("pid")
        state.pop("started", None)
        write_state(config, state)


def free_port(config: Config) -> None:
    """Stop a previous Rift backend. Anything else on the port is left alone."""
    previous = managed(config)
    if previous is not None:
        say(f"Stopping the previous Rift backend (PID {previous.pid})")
        stop_pid(previous.pid)
    clear_pid(config)  # also drops a stale PID whose process is gone

    for pid in listeners(config.port):
        proc = proc_info(pid)
        if proc is None:
            continue
        if not proc.is_rift:
            raise DemoError(
                f"port {config.port} is in use by PID {pid} ({Path(proc.command).name}), "
                "which is not a Rift backend. It was left running.\n"
                f"Stop it yourself (kill {pid}) and run the command again."
            )
        say(f"Stopping a Rift backend that is using port {config.port} (PID {pid})")
        stop_pid(pid)
    if port_open(config.probe_host, config.port):
        raise DemoError(
            f"port {config.port} is still in use and its owner could not be identified "
            "as a Rift backend. Nothing was stopped."
        )


def backend_command(config: Config) -> list[str]:
    """Build (a no-op when up to date) and return the backend binary to run."""
    binary = config.root / "backend" / "target" / "debug" / BACKEND_NAME
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    if not Path(cargo).is_file():
        if binary.is_file():
            say("cargo not found: using the existing backend binary")
            return [str(binary)]
        raise DemoError("cargo is not installed and there is no built backend: see docs/SETUP.md")
    command = [
        cargo,
        "build",
        "--quiet",
        "--manifest-path",
        str(config.root / "backend/Cargo.toml"),
    ]
    features = config.env.get("RIFT_DEMO_FEATURES", "").strip()
    if features:
        command += ["--features", features]
    if not binary.is_file():
        say("Building the backend (the first build takes a few minutes)...")
    try:
        result = subprocess.run(  # noqa: S603
            command,
            capture_output=True,
            text=True,
            timeout=float(config.env.get("RIFT_DEMO_BUILD_TIMEOUT") or 1200),
            check=False,
        )
    except subprocess.TimeoutExpired:
        raise DemoError("the backend build timed out") from None
    if result.returncode != 0 or not binary.is_file():
        detail = "\n".join(result.stderr.strip().splitlines()[-15:])
        raise DemoError(f"the backend did not build:\n{detail}")
    return [str(binary)]


# --- readiness: the real protocol ---------------------------------------------


def _read(sock: socket.socket, count: int) -> bytes:
    data = b""
    while len(data) < count:
        chunk = sock.recv(count - len(data))
        if not chunk:
            raise OSError("connection closed")
        data += chunk
    return data


def _ws_send(sock: socket.socket, text: str) -> None:
    payload = text.encode()
    mask = os.urandom(4)
    size = len(payload)
    if size < 126:
        head = struct.pack(">BB", 0x81, 0x80 | size)
    elif size < 65536:
        head = struct.pack(">BBH", 0x81, 0x80 | 126, size)
    else:
        head = struct.pack(">BBQ", 0x81, 0x80 | 127, size)
    sock.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))


def _ws_recv(sock: socket.socket) -> str:
    while True:
        first, second = _read(sock, 2)
        size = second & 0x7F
        if size == 126:
            (size,) = struct.unpack(">H", _read(sock, 2))
        elif size == 127:
            (size,) = struct.unpack(">Q", _read(sock, 8))
        data = _read(sock, size)
        opcode = first & 0x0F
        if opcode == 1:
            return data.decode()
        if opcode == 8:
            raise OSError("websocket closed by the backend")


def _request(sock: socket.socket, message_type: str, payload: dict[str, Any]) -> dict[str, Any]:
    message_id = str(uuid.uuid4())
    envelope = {
        "protocol_version": PROTOCOL_VERSION,
        "message_id": message_id,
        "message_type": message_type,
        "timestamp": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "session_id": None,
        "payload": payload,
    }
    _ws_send(sock, json.dumps(envelope))
    for _ in range(20):
        reply = json.loads(_ws_recv(sock))
        if reply.get("reply_to") == message_id:
            return reply
    raise OSError(f"no reply to {message_type}")


def protocol_probe(host: str, port: int, expect_location: str | None) -> None:
    """hello -> hello_ack, create_session -> session_created, over a real socket.

    With a world the new session is placed at the scenario's starting location,
    which is the proof that the story started. Raises OSError otherwise.
    """
    with socket.create_connection((host, port), timeout=3) as sock:
        sock.settimeout(5)
        key = base64.b64encode(os.urandom(16)).decode()
        sock.sendall(
            (
                f"GET /ws HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        response = b""
        while b"\r\n\r\n" not in response:
            response += _read(sock, 1)
        if b" 101 " not in response.split(b"\r\n", 1)[0]:
            raise OSError("the backend refused the WebSocket upgrade")

        ack = _request(sock, "hello", {"client": "rift-demo-launcher", "client_version": "1"})
        if ack.get("message_type") != "hello_ack":
            raise OSError(f"hello was answered with {ack.get('message_type')}")
        created = _request(sock, "create_session", {})
        if created.get("message_type") != "session_created":
            raise OSError(f"create_session was answered with {created.get('message_type')}")
        location = (created.get("payload") or {}).get("current_location")
        if expect_location and location != expect_location:
            raise OSError("the session did not start the story (world not loaded)")
        sock.sendall(b"\x88\x80" + os.urandom(4))  # close frame: a clean goodbye
        with contextlib.suppress(OSError):
            _ws_recv(sock)


def health_ok(host: str, port: int) -> bool:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(f"http://{host}:{port}/health", timeout=2) as response:
            return json.load(response).get("service") == BACKEND_NAME
    except (OSError, ValueError):
        return False


def wait_ready(
    config: Config, child: subprocess.Popen[bytes], expect_location: str | None, timeout: float
) -> str | None:
    """None once the backend answers the protocol; otherwise what went wrong."""
    deadline = time.monotonic() + timeout
    last = "it never answered /health"
    while time.monotonic() < deadline:
        code = child.poll()
        if code is not None:
            return f"the backend exited during startup (exit code {code})"
        if health_ok(config.probe_host, config.port):
            try:
                protocol_probe(config.probe_host, config.port, expect_location)
                return None
            except (OSError, ValueError) as exc:
                last = str(exc)
        time.sleep(0.2)
    return f"the backend was not ready after {timeout:g}s ({last})"


def log_tail(config: Config, lines: int) -> list[str]:
    try:
        text = config.log.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return []
    return text.splitlines()[-lines:]


# --- Unreal ---------------------------------------------------------------------


def unreal_running() -> bool:
    pgrep = shutil.which("pgrep")
    if pgrep is None:
        return False
    result = subprocess.run([pgrep, "-x", "UnrealEditor"], capture_output=True, check=False)  # noqa: S603
    return result.returncode == 0


def open_unreal(config: Config) -> str:
    """Open the one Unreal project (the same client for every universe)."""
    if unreal_running():
        return "already open (press Play)"
    if not config.uproject.is_file():
        return f"NOT OPENED, project not found: {config.uproject}"
    opener = shutil.which("open")
    if opener is None:
        return f"NOT OPENED (no `open` command): open {config.uproject} yourself"
    subprocess.run([opener, str(config.uproject)], check=False)  # noqa: S603
    return "opening the editor (press Play when it is up)"


# --- commands -------------------------------------------------------------------


def _summary(universe: Universe, scenario: dict[str, Any]) -> dict[str, str]:
    bible = universe.bible
    location_id = (scenario.get("world") or {}).get("player_location") or (
        bible.get("starting_location") or {}
    ).get("location_id", "")
    names = {item.get("id"): item.get("name") for item in bible.get("locations", [])}
    missions = (scenario.get("plan") or {}).get("missions") or [{}]
    return {
        "universe": universe.title,
        "role": (bible.get("player_role") or {}).get("title", "?"),
        "location_id": location_id,
        "location": names.get(location_id) or location_id or "?",
        "mission": missions[0].get("title", "?"),
    }


def cmd_start(config: Config, args: argparse.Namespace) -> int:
    if args.new:
        try:
            universe = resolve_title(config, args.title, exact=True)
            say(f"{universe.title} is already compiled: using the cached files")
        except DemoError:
            universe = compile_title(config, args.title)
    else:
        universe = resolve_title(config, args.title)
    scenario_path = find_scenario(config, universe)
    if scenario_path is None:
        say(f"No scenario yet for {universe.title}: building one from the WorldBible")
        scenario_path = build_scenario(config, universe)
    scenario = _load_json(scenario_path)
    if not _fits(scenario, universe):
        raise DemoError(f"{scenario_path} is not a scenario for {universe.title}")
    summary = _summary(universe, scenario)

    free_port(config)
    command = backend_command(config)
    env = {
        **config.env,
        "RIFT_WORLD_BIBLE": str(universe.bible_path.resolve()),
        "RIFT_SCENARIO": str(scenario_path.resolve()),
        "BACKEND_HOST": config.host,
        "BACKEND_PORT": str(config.port),
        "NO_COLOR": "1",
    }
    config.demo_dir.mkdir(parents=True, exist_ok=True)
    with config.log.open("wb") as log:
        child = subprocess.Popen(  # noqa: S603
            command,
            cwd=config.root,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    proc = proc_info(child.pid)
    write_state(
        config,
        {
            "pid": child.pid,
            "started": proc.started if proc else None,
            "universe": universe.title,
            "universe_id": universe.universe_id,
            "world_bible": env["RIFT_WORLD_BIBLE"],
            "scenario": env["RIFT_SCENARIO"],
            "port": config.port,
        },
    )
    timeout = float(config.env.get("RIFT_DEMO_TIMEOUT") or 45)
    try:
        problem = wait_ready(config, child, summary["location_id"] or None, timeout)
    except KeyboardInterrupt:
        problem = "interrupted"
    if problem is not None:
        stop_pid(child.pid)
        clear_pid(config)
        say(f"RIFT FAILED TO START: {problem}")
        tail = log_tail(config, 20)
        if tail:
            say("--- last backend log lines ---")
            say("\n".join(tail))
        say("Full log: make demo-logs")
        return 1

    unreal = "skipped (--no-unreal)" if args.no_unreal else open_unreal(config)
    bar = "=" * 44
    say(bar)
    say("RIFT READY")
    say(f"Universe: {summary['universe']}")
    say(f"Player Role: {summary['role']}")
    say(f"Location: {summary['location']}")
    say(f"Mission: {summary['mission']}")
    say(f"Backend: READY (ws://{config.probe_host}:{config.port}/ws, PID {child.pid})")
    say(f"Director: Gemini {presence(config, 'GEMINI_API_KEY')}")
    say(f"Voice: ElevenLabs {presence(config, 'ELEVENLABS_API_KEY')}")
    say(f"Unreal: {unreal}")
    say(bar)
    return 0


def cmd_stop(config: Config, _args: argparse.Namespace) -> int:
    proc = managed(config)
    if proc is None:
        clear_pid(config)
        say("Rift backend: not running (nothing to stop)")
        return 0
    stop_pid(proc.pid)
    if proc_info(proc.pid) is not None:
        say(f"Rift backend: PID {proc.pid} did not stop")
        return 1
    clear_pid(config)
    say(f"Rift backend: stopped (PID {proc.pid})")
    return 0


def cmd_status(config: Config, _args: argparse.Namespace) -> int:
    state = read_state(config)
    proc = managed(config)
    if proc is not None:
        say(f"Backend: running (PID {proc.pid}, port {state.get('port', config.port)})")
    else:
        others = [p for p in map(proc_info, listeners(config.port)) if p is not None and p.is_rift]
        if others:
            say(f"Backend: running, not started by the launcher (PID {others[0].pid})")
            return 0
        say("Backend: stopped")
    if state.get("universe"):
        label = "Universe" if proc is not None else "Last universe"
        say(f"{label}: {state['universe']}")
        say(f"WorldBible: {state.get('world_bible')}")
        say(f"Scenario: {state.get('scenario')}")
    return 0


def cmd_logs(config: Config, args: argparse.Namespace) -> int:
    tail = log_tail(config, args.lines)
    if not tail:
        say(f"No backend log yet ({config.log})")
        return 0
    say(f"--- {config.log} (last {len(tail)} lines) ---")
    say("\n".join(tail))
    return 0


def cmd_preflight(config: Config, _args: argparse.Namespace) -> int:
    """Local checks only: no network request, no API credit."""
    ok = True

    def check(label: str, good: bool, detail: str = "") -> None:
        nonlocal ok
        ok = ok and good
        say(f"  {'OK     ' if good else 'MISSING'} {label}{f' - {detail}' if detail else ''}")

    say("Rift demo preflight (offline)")
    binary = config.root / "backend" / "target" / "debug" / BACKEND_NAME
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
    check("cargo", Path(cargo).is_file() or binary.is_file())
    built = "built" if binary.is_file() else "not built (the first launch builds it)"
    say(f"  backend binary: {built}")
    check("uv (Scenario Builder, demo-new)", shutil.which("uv") is not None)
    check("Unreal project", config.uproject.is_file(), str(config.uproject))
    say(f"  .env: {config.env_file or 'none found'}")
    for key in REPORTED_KEYS:
        say(f"    {key}: {presence(config, key)}")
    say("  Universes ready to launch:")
    seen: set[str] = set()
    for universe in available_universes(config):
        if universe.universe_id in seen:
            continue
        seen.add(universe.universe_id)
        has = "scenario cached" if find_scenario(config, universe) else "scenario built on launch"
        say(f"    {universe.title} ({has})")
    check("at least one universe", bool(seen))
    owners = [proc_info(pid) for pid in listeners(config.port)]
    foreign = [p for p in owners if p is not None and not p.is_rift and managed(config) is None]
    check(
        f"port {config.port}",
        not foreign,
        f"in use by {Path(foreign[0].command).name} (PID {foreign[0].pid})" if foreign else "",
    )
    say("PREFLIGHT OK" if ok else "PREFLIGHT FAILED")
    return 0 if ok else 1


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="rift_demo", description="Rift demo launcher.")
    commands = parser.add_subparsers(dest="command", required=True)
    start = commands.add_parser("start", help="launch a universe")
    start.add_argument("--title", default="", help='e.g. "The Matrix"')
    start.add_argument("--no-unreal", action="store_true", help="backend only")
    start.add_argument("--new", action="store_true", help="compile the title first if needed")
    start.set_defaults(run=cmd_start)
    commands.add_parser("stop", help="stop the backend").set_defaults(run=cmd_stop)
    commands.add_parser("status", help="what is running").set_defaults(run=cmd_status)
    logs = commands.add_parser("logs", help="tail of the backend log")
    logs.add_argument("-n", "--lines", type=int, default=40)
    logs.set_defaults(run=cmd_logs)
    commands.add_parser("preflight", help="offline checks").set_defaults(run=cmd_preflight)
    return parser


def main(
    argv: list[str] | None = None, root: Path = ROOT, environ: dict[str, str] | None = None
) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.run(load_config(root, environ), args)
    except DemoError as exc:
        say(f"ERROR: {exc}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
