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


def _live_status(result: LiveResult) -> str:
    if result.ok is None:
        return f"live: SKIPPED ({result.detail})"
    return f"live: {'PASS' if result.ok else 'FAIL'} ({result.detail})"


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
        configured = provider.is_configured()
        detail = "" if configured else f"set {', '.join(provider.env_vars)}"
        if result is not None:
            detail = _live_status(result) if configured else detail
            if result.ok is False:
                healthy = False
        lines.append(_line(provider.name, "CONFIGURED" if configured else "MISSING", detail))

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
