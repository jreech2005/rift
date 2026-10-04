"""External service providers. Every external service sits behind ``Provider``."""

from rift_canon.config import Settings
from rift_canon.providers.base import LiveResult, Provider
from rift_canon.providers.claude import ClaudeProvider
from rift_canon.providers.elevenlabs import ElevenLabsProvider
from rift_canon.providers.tidb import TiDBProvider
from rift_canon.providers.tmdb import TMDBProvider
from rift_canon.providers.worldlabs import WorldLabsProvider

PROVIDER_TYPES: tuple[type[Provider], ...] = (
    TMDBProvider,
    ClaudeProvider,
    TiDBProvider,
    ElevenLabsProvider,
    WorldLabsProvider,
)


def all_providers(settings: Settings) -> list[Provider]:
    return [cls(settings) for cls in PROVIDER_TYPES]


__all__ = [
    "PROVIDER_TYPES",
    "ClaudeProvider",
    "ElevenLabsProvider",
    "LiveResult",
    "Provider",
    "TMDBProvider",
    "TiDBProvider",
    "WorldLabsProvider",
    "all_providers",
]
