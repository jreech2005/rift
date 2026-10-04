"""Rift canon doctor: report environment and provider configuration.

    uv run python -m rift_canon.doctor          # offline: configuration only
    uv run python -m rift_canon.doctor --live   # + free, read-only connectivity checks

Never prints secret values.
"""

from __future__ import annotations

import argparse
import asyncio
import platform
import sys
from collections.abc import Sequence

from rift_canon.config import Settings, load_settings
from rift_canon.providers import Provider, all_providers
from rift_canon.providers.base import LiveResult, make_client

MIN_PYTHON = (3, 12)
LABEL_WIDTH = 24


def _line(label: str, status: str, detail: str = "") -> str:
    return f"{label:<{LABEL_WIDTH}}{status:<12}{detail}".rstrip()


def _provider_line(provider: Provider, settings: Settings, result: LiveResult | None) -> str:
    """MISSING (not configured), CONFIGURED (not live-tested), TESTED or FAILED (live check)."""
    if not provider.is_configured():
        # Names only; values are never read here.
        missing = [v for v in provider.env_vars if getattr(settings, v.lower(), None) is None]
        return _line(provider.name, "MISSING", f"set {', '.join(missing)}")
    if result is None:
        return _line(provider.name, "CONFIGURED")
    if result.ok is None:
        return _line(provider.name, "CONFIGURED", result.detail)
    return _line(provider.name, "TESTED" if result.ok else "FAILED", result.detail)


async def _run_live(providers: Sequence[Provider]) -> list[LiveResult]:
    async with make_client() as client:
        return list(await asyncio.gather(*(p.run_live_check(client) for p in providers)))


def report(settings: Settings, live: bool = False) -> tuple[list[str], bool]:
    """Build doctor output lines. Returns (lines, healthy)."""
    lines = ["Rift doctor (canon)", ""]
    healthy = True

    py_ok = sys.version_info >= MIN_PYTHON
    healthy &= py_ok
    lines.append(_line("Python", "PASS" if py_ok else "WARN", platform.python_version()))

    if settings.env_file is not None:
        lines.append(_line("Environment", "PASS", f".env loaded ({settings.env_file.name})"))
    else:
        lines.append(_line("Environment", "WARN", "no .env found; copy .env.example to .env"))

    providers = all_providers(settings)
    results = asyncio.run(_run_live(providers)) if live else [None] * len(providers)
    for provider, result in zip(providers, results, strict=True):
        if result is not None and result.ok is False:
            healthy = False
        lines.append(_provider_line(provider, settings, result))

    if not live:
        lines += ["", "Run with --live for free, read-only connectivity checks."]
    return lines, healthy


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="rift_canon.doctor", description=__doc__)
    parser.add_argument(
        "--live", action="store_true", help="run harmless connectivity checks (no paid calls)"
    )
    args = parser.parse_args(argv)

    lines, healthy = report(load_settings(), live=args.live)
    print("\n".join(lines))
    return 0 if healthy else 1


if __name__ == "__main__":
    sys.exit(main())
