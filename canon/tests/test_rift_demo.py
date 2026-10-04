"""The demo launcher (``scripts/rift_demo.py``). Offline: the backend is a local fake.

Every test runs in a copy of the repo layout whose path contains a space.
"""

import importlib.util
import json
import shutil
import socket
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

REPO = Path(__file__).parents[2]
FIXTURES = REPO / "backend" / "tests" / "fixtures"
FAKE_BACKEND = Path(__file__).with_name("fake_backend.py")
SECRET = "fake-gemini-secret-111"  # noqa: S105
DB_URL = "postgres://tsdbadmin:fake-tiger-secret-777@db.example:5432/tsdb"

spec = importlib.util.spec_from_file_location("rift_demo", REPO / "scripts" / "rift_demo.py")
rift_demo = importlib.util.module_from_spec(spec)
sys.modules["rift_demo"] = rift_demo
spec.loader.exec_module(rift_demo)

HARRY_POTTER = "Harry Potter and the Philosopher's Stone"


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Demo:
    """A throwaway Rift checkout plus the launcher pointed at it."""

    def __init__(self, tmp_path: Path) -> None:
        self.root = tmp_path / "rift demo"
        self.cache = self.root / "cache" / "universes"
        self.cache.mkdir(parents=True)
        (self.root / "canon").mkdir()
        for name in ("director", "runtime", "scenario_builder"):
            shutil.copytree(FIXTURES / name, self.root / "backend/tests/fixtures" / name)
        (self.root / ".env").write_text(
            f"GEMINI_API_KEY={SECRET}\nTIGER_DATABASE_URL='{DB_URL}'\nELEVENLABS_API_KEY=\n"
        )
        self.port = _free_port()
        self.environ = {
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "BACKEND_PORT": str(self.port),
            "RIFT_DEMO_HOME": str(tmp_path / "no such home"),
            "RIFT_DEMO_TIMEOUT": "15",
        }
        self.opened: list[Path] = []

    def run(self, *argv: str) -> int:
        return rift_demo.main(list(argv), root=self.root, environ=self.environ)

    def start(self, title: str, *extra: str) -> int:
        return self.run("start", "--title", title, "--no-unreal", *extra)

    @property
    def state(self) -> dict[str, Any]:
        return json.loads((self.root / ".demo" / "state.json").read_text())

    @property
    def log(self) -> str:
        return (self.root / ".demo" / "rift-backend.log").read_text()


@pytest.fixture
def demo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[Demo]:
    demo = Demo(tmp_path)
    monkeypatch.setattr(
        rift_demo, "backend_command", lambda config: [sys.executable, str(FAKE_BACKEND)]
    )
    monkeypatch.setattr(rift_demo, "uv_command", lambda: [sys.executable])
    monkeypatch.setattr(
        rift_demo, "open_unreal", lambda config: demo.opened.append(config.uproject) or "opened"
    )
    yield demo
    demo.run("stop")


def _alive(pid: int) -> bool:
    return rift_demo.proc_info(pid) is not None


# --- title resolution ---------------------------------------------------------


@pytest.mark.parametrize(
    ("title", "universe_id", "bible", "scenario"),
    [
        (
            "Breaking Bad",
            "breaking_bad_tv_1396",
            "director/world_bible_breaking_bad.json",
            "runtime/scenario_burner_phone.json",
        ),
        (
            "The Matrix",
            "the_matrix_movie_603",
            "scenario_builder/the_matrix.world.json",
            "scenario_builder/the_matrix.scenario.json",
        ),
        (
            HARRY_POTTER,
            "harry_potter_and_the_philosopher_s_stone_movie_671",
            "scenario_builder/harry_potter_1.world.json",
            "scenario_builder/harry_potter_1.scenario.json",
        ),
    ],
)
def test_resolves_a_title_to_its_world_bible_and_scenario(
    demo: Demo, title: str, universe_id: str, bible: str, scenario: str
) -> None:
    config = rift_demo.load_config(demo.root, demo.environ)
    universe = rift_demo.resolve_title(config, title)
    fixtures = demo.root / "backend/tests/fixtures"
    assert universe.universe_id == universe_id
    assert universe.bible_path == fixtures / bible
    assert rift_demo.find_scenario(config, universe) == fixtures / scenario


@pytest.mark.parametrize(
    "title", ["the matrix", "MATRIX", "Harry Potter", "harry potter and the philosophers stone"]
)
def test_titles_are_matched_loosely_on_universe_metadata(demo: Demo, title: str) -> None:
    config = rift_demo.load_config(demo.root, demo.environ)
    assert rift_demo.resolve_title(config, title).universe_id.endswith(("_603", "_671"))


def test_a_cached_world_bible_is_found_whatever_its_file_name(demo: Demo) -> None:
    shutil.copy(FIXTURES / "scenario_builder/the_matrix.world.json", demo.cache / "anything.json")
    config = rift_demo.load_config(demo.root, demo.environ)
    # The cache comes before the checked-in fixtures, and a scenario elsewhere that
    # fits the same universe is still found.
    universe = rift_demo.resolve_title(config, "The Matrix")
    assert universe.bible_path == demo.cache / "anything.json"
    scenario = rift_demo.find_scenario(config, universe)
    assert scenario is not None and scenario.name == "the_matrix.scenario.json"


def test_unknown_title_fails_without_starting_anything(
    demo: Demo, capsys: pytest.CaptureFixture[str]
) -> None:
    assert demo.start("Blade Runner") == 1
    out = capsys.readouterr().out
    assert 'no compiled universe matches "Blade Runner"' in out
    assert "make demo-new" in out
    assert not (demo.root / ".demo").exists()


def test_a_missing_title_is_an_error(demo: Demo, capsys: pytest.CaptureFixture[str]) -> None:
    assert demo.start("") == 1
    assert "a title is required" in capsys.readouterr().out


# --- launch ---------------------------------------------------------------------


def test_launch_uses_the_cached_scenario_and_reports_ready(
    demo: Demo, capsys: pytest.CaptureFixture[str]
) -> None:
    assert demo.start("The Matrix") == 0
    out = capsys.readouterr().out
    assert "RIFT READY" in out
    assert "Universe: The Matrix" in out
    assert "Player Role: Assistant Hovercraft Engineer" in out
    assert "Location: Nebuchadnezzar" in out
    assert "Mission: Power Surge in the Simulation Deck" in out
    assert "Backend: READY" in out
    assert "{" not in out  # no raw JSON
    state = demo.state
    assert state["scenario"].endswith("scenario_builder/the_matrix.scenario.json")
    assert state["world_bible"].endswith("scenario_builder/the_matrix.world.json")
    assert _alive(state["pid"])
    assert "world loaded universe_id=the_matrix_movie_603" in demo.log
    assert "story started" in demo.log
    assert not list(demo.cache.iterdir())  # nothing was generated


def test_scenario_is_built_when_missing(demo: Demo, capsys: pytest.CaptureFixture[str]) -> None:
    builder = demo.root / "backend/tests/fixtures/scenario_builder"
    shutil.move(builder / "harry_potter_1.world.json", demo.cache / "harry potter.world.json")
    shutil.rmtree(builder)

    assert demo.start(HARRY_POTTER) == 0
    out = capsys.readouterr().out
    assert "building one from the WorldBible" in out
    assert "Mission: The Crowded Threshold" in out
    built = demo.cache / "harry potter.scenario.json"
    assert demo.state["scenario"] == str(built.resolve())
    golden = FIXTURES / "scenario_builder/harry_potter_1.scenario.json"
    assert json.loads(built.read_text()) == json.loads(golden.read_text())


def test_no_unreal_skips_the_editor_and_the_default_opens_it(demo: Demo) -> None:
    assert demo.start("Breaking Bad") == 0
    assert demo.opened == []
    assert demo.run("start", "--title", "Breaking Bad") == 0
    assert demo.opened == [Path(demo.environ["RIFT_DEMO_HOME"]) / "game/Rift/Rift.uproject"]


def test_relaunch_replaces_the_previous_backend(demo: Demo) -> None:
    assert demo.start("The Matrix") == 0
    first = demo.state["pid"]
    assert demo.start(HARRY_POTTER) == 0
    assert not _alive(first)
    assert _alive(demo.state["pid"])
    assert demo.state["universe"] == HARRY_POTTER


def test_a_stale_pid_is_dropped_and_its_new_owner_is_not_killed(demo: Demo) -> None:
    bystander = subprocess.Popen(["/bin/sleep", "60"])  # noqa: S603
    try:
        (demo.root / ".demo").mkdir()
        stale = {"pid": bystander.pid, "started": "Thu Jan  1 00:00:00 1970", "universe": "Old"}
        (demo.root / ".demo" / "state.json").write_text(json.dumps(stale))

        assert demo.run("stop") == 0
        assert bystander.poll() is None
        assert "pid" not in demo.state

        (demo.root / ".demo" / "state.json").write_text(json.dumps(stale))
        assert demo.start("The Matrix") == 0
        assert bystander.poll() is None
        assert demo.state["pid"] != bystander.pid
    finally:
        bystander.kill()
        bystander.wait()


def test_an_unrelated_process_on_the_port_is_not_killed(
    demo: Demo, capsys: pytest.CaptureFixture[str]
) -> None:
    code = "import socket,sys,time; s=socket.socket(); s.bind(('127.0.0.1',int(sys.argv[1])));"
    other = subprocess.Popen(  # noqa: S603
        [
            sys.executable,
            "-c",
            code + "s.listen(); print('up', flush=True); time.sleep(60)",
            str(demo.port),
        ],
        stdout=subprocess.PIPE,
    )
    try:
        assert other.stdout is not None and other.stdout.readline().strip() == b"up"
        assert demo.start("The Matrix") == 1
        out = capsys.readouterr().out
        assert f"port {demo.port} is in use by PID {other.pid}" in out
        assert "not a Rift backend" in out
        assert other.poll() is None
        assert not (demo.root / ".demo" / "rift-backend.log").exists()
    finally:
        other.kill()
        other.wait()


def test_startup_timeout_stops_the_child_and_shows_the_log(
    demo: Demo, capsys: pytest.CaptureFixture[str]
) -> None:
    demo.environ |= {"FAKE_MODE": "hang", "RIFT_DEMO_TIMEOUT": "1"}
    assert demo.start("The Matrix") == 1
    out = capsys.readouterr().out
    assert "RIFT FAILED TO START: the backend was not ready after 1s" in out
    assert "starting very slowly" in out
    assert "pid" not in demo.state
    assert rift_demo.listeners(demo.port) == []


def test_a_backend_that_exits_fails_fast_without_leaking_secrets(
    demo: Demo, capsys: pytest.CaptureFixture[str]
) -> None:
    demo.environ["FAKE_MODE"] = "crash"
    assert demo.start("The Matrix") == 1
    out = capsys.readouterr().out
    assert "exited during startup (exit code 3)" in out
    assert "cannot start, key=***" in out
    assert SECRET not in out
    assert demo.run("logs") == 0
    assert SECRET not in capsys.readouterr().out


# --- stop, status, logs, preflight ---------------------------------------------------


def test_stop_when_running_and_when_already_stopped(
    demo: Demo, capsys: pytest.CaptureFixture[str]
) -> None:
    assert demo.start("The Matrix") == 0
    pid = demo.state["pid"]
    assert demo.run("stop") == 0
    assert f"stopped (PID {pid})" in capsys.readouterr().out
    assert not _alive(pid)

    assert demo.run("stop") == 0
    assert "not running" in capsys.readouterr().out


def test_stop_with_no_demo_directory(demo: Demo, capsys: pytest.CaptureFixture[str]) -> None:
    assert demo.run("stop") == 0
    assert "not running" in capsys.readouterr().out
    assert demo.run("status") == 0
    assert "Backend: stopped" in capsys.readouterr().out
    assert demo.run("logs") == 0


def test_status_reports_the_loaded_universe(demo: Demo, capsys: pytest.CaptureFixture[str]) -> None:
    assert demo.start(HARRY_POTTER) == 0
    capsys.readouterr()
    assert demo.run("status") == 0
    out = capsys.readouterr().out
    assert f"Backend: running (PID {demo.state['pid']}" in out
    assert f"Universe: {HARRY_POTTER}" in out
    assert "WorldBible: " in out and "harry_potter_1.world.json" in out
    assert "Scenario: " in out and "harry_potter_1.scenario.json" in out


def test_nothing_prints_a_secret(demo: Demo, capsys: pytest.CaptureFixture[str]) -> None:
    assert demo.run("preflight") in (0, 1)
    assert demo.start("Breaking Bad") == 0
    for command in ("status", "logs", "stop"):
        assert demo.run(command) == 0
    out = capsys.readouterr().out
    assert "GEMINI_API_KEY: PRESENT" in out
    assert "TIGER_DATABASE_URL: PRESENT" in out
    assert "ELEVENLABS_API_KEY: MISSING" in out
    for secret in (SECRET, DB_URL, "fake-tiger-secret-777"):
        assert secret not in out
    assert SECRET not in (demo.root / ".demo" / "state.json").read_text()


def test_env_file_parsing(tmp_path: Path) -> None:
    path = tmp_path / ".env"
    path.write_text("# c\nA=1\nexport B='two words'\nC=\"q\" \nD=x # note\nE=\n\nnot a line\n")
    assert rift_demo.parse_env_file(path) == {
        "A": "1",
        "B": "two words",
        "C": "q",
        "D": "x",
        "E": "",
    }
