import pytest

from rift_canon import doctor
from rift_canon.config import Settings
from tests.conftest import SECRET_VALUES


def _status(lines: list[str], label: str) -> str:
    line = next(line for line in lines if line.startswith(label + " "))
    return line[doctor.LABEL_WIDTH :].split()[0]


def test_reports_missing(empty_settings: Settings) -> None:
    lines, healthy = doctor.report(empty_settings)
    assert healthy
    assert _status(lines, "Python") == "PASS"
    assert _status(lines, "Environment") == "WARN"
    for name in ("TMDB", "Gemini", "TiDB", "ElevenLabs", "World Labs"):
        assert _status(lines, name) == "MISSING"


def test_reports_configured_without_secrets(full_settings: Settings) -> None:
    lines, _ = doctor.report(full_settings)
    for name in ("TMDB", "Gemini", "TiDB", "ElevenLabs", "World Labs"):
        assert _status(lines, name) == "CONFIGURED"
    output = "\n".join(lines)
    for secret in SECRET_VALUES:
        assert secret not in output


def test_main_never_prints_secrets(monkeypatch, capsys: pytest.CaptureFixture[str]) -> None:
    from tests.conftest import FAKE_SECRETS

    for key, value in FAKE_SECRETS.items():
        monkeypatch.setenv(key, value)
    assert doctor.main([]) == 0
    out = capsys.readouterr().out
    assert "CONFIGURED" in out
    for secret in SECRET_VALUES:
        assert secret not in out
