import pytest

from rift_canon.config import Settings

FAKE_SECRETS = {
    "GEMINI_API_KEY": "fake-gemini-secret-111",
    "TMDB_API_KEY": "fake-tmdb-secret-222",
    "WORLD_LABS_API_KEY": "fake-worldlabs-secret-333",
    "ELEVENLABS_API_KEY": "fake-eleven-secret-444",
    "TIDB_HOST": "127.0.0.1",
    "TIDB_PORT": "4000",
    "TIDB_USER": "rift",
    "TIDB_PASSWORD": "fake-tidb-secret-555",
    "TIDB_DATABASE": "rift",
}

SECRET_VALUES = [v for k, v in FAKE_SECRETS.items() if "secret" in v]


@pytest.fixture
def full_settings() -> Settings:
    return Settings.from_mapping(FAKE_SECRETS)


@pytest.fixture
def empty_settings() -> Settings:
    return Settings.from_mapping({})
