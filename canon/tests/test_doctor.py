import httpx
import pytest

from rift_canon import doctor
from rift_canon.config import Settings
from tests.conftest import FAKE_SECRETS, SECRET_VALUES
from tests.support import sdk_client


def _status(lines: list[str], label: str) -> str:
    line = next(line for line in lines if line.startswith(label + " "))
    return line[doctor.LABEL_WIDTH :].split()[0]


def test_reports_missing(empty_settings: Settings) -> None:
    lines, healthy = doctor.report(empty_settings)
    assert healthy
    assert _status(lines, "Python") == "PASS"
    assert _status(lines, "Environment") == "WARN"
    for name in ("TMDB", "Claude", "TiDB", "ElevenLabs", "World Labs"):
        assert _status(lines, name) == "MISSING"


def test_reports_configured_without_secrets(full_settings: Settings) -> None:
    lines, _ = doctor.report(full_settings)
    for name in ("TMDB", "Claude", "TiDB", "ElevenLabs", "World Labs"):
        assert _status(lines, name) == "CONFIGURED"
    output = "\n".join(lines)
    for secret in SECRET_VALUES:
        assert secret not in output


def test_main_never_prints_secrets(monkeypatch, capsys: pytest.CaptureFixture[str]) -> None:
    for key, value in FAKE_SECRETS.items():
        monkeypatch.setenv(key, value)
    assert doctor.main([]) == 0
    out = capsys.readouterr().out
    assert "CONFIGURED" in out
    for secret in SECRET_VALUES:
        assert secret not in out


def _live_report(monkeypatch, settings: Settings, handler) -> tuple[list[str], bool]:
    """Run the live doctor against a mock transport (TiDB's TCP check is stubbed)."""
    from rift_canon.providers import ClaudeProvider, TiDBProvider
    from rift_canon.providers.base import LiveResult

    async def tidb_ok(self, client) -> LiveResult:
        return LiveResult(True, "TCP reachable (auth not tested)")

    monkeypatch.setattr(TiDBProvider, "live_check", tidb_ok)
    monkeypatch.setattr(ClaudeProvider, "http_client", sdk_client(handler))
    monkeypatch.setattr(
        doctor, "make_client", lambda: httpx.AsyncClient(transport=httpx.MockTransport(handler))
    )
    return doctor.report(settings, live=True)


def test_live_success_reports_tested(monkeypatch, full_settings: Settings) -> None:
    def ok(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"results": [], "models": [], "data": []})

    lines, healthy = _live_report(monkeypatch, full_settings, ok)
    assert healthy
    for name in ("TMDB", "Claude", "TiDB", "ElevenLabs"):
        assert _status(lines, name) == "TESTED"
    # No free endpoint: World Labs is never live-tested, so it stays CONFIGURED.
    assert _status(lines, "World Labs") == "CONFIGURED"


def test_live_failure_reports_failed_without_secrets(monkeypatch, full_settings: Settings) -> None:
    def failing(request: httpx.Request) -> httpx.Response:
        if request.url.host == "api.themoviedb.org":
            raise httpx.ConnectTimeout(f"timed out connecting to {request.url}")
        return httpx.Response(401, text=f"bad credentials {dict(request.headers)}")

    lines, healthy = _live_report(monkeypatch, full_settings, failing)
    assert not healthy
    for name in ("TMDB", "Claude", "ElevenLabs"):
        assert _status(lines, name) == "FAILED"
    output = "\n".join(lines)
    assert "timeout" in next(line for line in lines if line.startswith("TMDB"))
    assert "HTTP 401" in output
    for secret in SECRET_VALUES:
        assert secret not in output


def test_live_missing_providers_make_no_requests(monkeypatch, empty_settings: Settings) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        raise AssertionError("unconfigured providers must not make requests")

    lines, healthy = _live_report(monkeypatch, empty_settings, handler)
    assert healthy
    for name in ("TMDB", "Claude", "TiDB", "ElevenLabs", "World Labs"):
        assert _status(lines, name) == "MISSING"


def test_partial_tidb_lists_only_missing_names() -> None:
    partial = {k: v for k, v in FAKE_SECRETS.items() if k not in ("TIDB_PASSWORD", "TIDB_PORT")}
    lines, _ = doctor.report(Settings.from_mapping(partial))
    line = next(line for line in lines if line.startswith("TiDB"))
    assert _status(lines, "TiDB") == "MISSING"
    assert line.endswith("set TIDB_PORT, TIDB_PASSWORD")
