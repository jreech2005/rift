"""Environment configuration.

Secrets are held as ``SecretStr`` so they never appear in reprs, logs or
doctor output. Values come from the process environment, falling back to the
repo-root ``.env`` (override the path with ``RIFT_ENV_FILE``).
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from pathlib import Path

from dotenv import dotenv_values
from pydantic import BaseModel, ConfigDict, SecretStr

REPO_ROOT = Path(__file__).resolve().parents[3]
DEFAULT_ENV_FILE = REPO_ROOT / ".env"


class Settings(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore")

    gemini_api_key: SecretStr | None = None
    gemini_model: str = "gemini-3.8-flash"
    tmdb_api_key: SecretStr | None = None
    world_labs_api_key: SecretStr | None = None
    elevenlabs_api_key: SecretStr | None = None

    tidb_host: str | None = None
    tidb_port: int | None = None
    tidb_user: str | None = None
    tidb_password: SecretStr | None = None
    tidb_database: str | None = None

    backend_host: str = "127.0.0.1"
    backend_port: int = 3000

    env_file: Path | None = None
    """The .env file that was loaded, if any."""

    @classmethod
    def from_mapping(cls, env: Mapping[str, str | None], env_file: Path | None = None) -> Settings:
        fields = {name.upper(): name for name in cls.model_fields if name != "env_file"}
        # Unset and blank values (e.g. `TMDB_API_KEY=`) both count as missing.
        values = {
            field: env[key].strip() for key, field in fields.items() if (env.get(key) or "").strip()
        }
        return cls(**values, env_file=env_file)


def load_settings(env_file: Path | None = None) -> Settings:
    """Load settings from the environment, falling back to the .env file."""
    path = env_file or Path(os.environ.get("RIFT_ENV_FILE", DEFAULT_ENV_FILE))
    file_values: dict[str, str | None] = dict(dotenv_values(path)) if path.is_file() else {}
    merged = {**file_values, **os.environ}
    return Settings.from_mapping(merged, env_file=path if path.is_file() else None)
