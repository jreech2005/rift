from pathlib import Path

from rift_canon.config import Settings, load_settings
from tests.conftest import FAKE_SECRETS, SECRET_VALUES


def test_empty_mapping_uses_defaults(empty_settings: Settings) -> None:
    assert empty_settings.anthropic_api_key is None
    assert empty_settings.tidb_port is None
    assert empty_settings.backend_host == "127.0.0.1"
    assert empty_settings.backend_port == 3000


def test_blank_values_count_as_missing() -> None:
    s = Settings.from_mapping({"TMDB_API_KEY": "   ", "BACKEND_PORT": "", "TIDB_PORT": ""})
    assert s.tmdb_api_key is None
    assert s.backend_port == 3000
    assert s.tidb_port is None


def test_reads_values(full_settings: Settings) -> None:
    assert full_settings.tidb_port == 4000
    assert full_settings.tmdb_api_key is not None
    assert full_settings.tmdb_api_key.get_secret_value() == FAKE_SECRETS["TMDB_API_KEY"]


def test_secrets_hidden_in_repr(full_settings: Settings) -> None:
    text = repr(full_settings) + str(full_settings) + full_settings.model_dump_json()
    for secret in SECRET_VALUES:
        assert secret not in text


def test_load_settings_reads_env_file(tmp_path: Path, monkeypatch) -> None:
    for key in FAKE_SECRETS:
        monkeypatch.delenv(key, raising=False)
    env_file = tmp_path / ".env"
    env_file.write_text("ANTHROPIC_API_KEY=from-file\nBACKEND_PORT=4100\n")
    s = load_settings(env_file)
    assert s.env_file == env_file
    assert s.anthropic_api_key is not None
    assert s.anthropic_api_key.get_secret_value() == "from-file"
    assert s.backend_port == 4100


def test_process_env_overrides_file(tmp_path: Path, monkeypatch) -> None:
    env_file = tmp_path / ".env"
    env_file.write_text("ANTHROPIC_API_KEY=from-file\n")
    monkeypatch.setenv("ANTHROPIC_API_KEY", "from-env")
    s = load_settings(env_file)
    assert s.anthropic_api_key is not None
    assert s.anthropic_api_key.get_secret_value() == "from-env"


def test_missing_env_file(tmp_path: Path) -> None:
    assert load_settings(tmp_path / "nope.env").env_file is None
