"""scripts/find_unreal.sh: filesystem-only Unreal Engine detection."""

import os
import subprocess
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "find_unreal.sh"


def _install(root: Path, name: str, editor: bool = True) -> Path:
    engine = root / name
    (engine / "Engine" / "Binaries" / "Mac").mkdir(parents=True)
    if editor:
        (engine / "Engine" / "Binaries" / "Mac" / "UnrealEditor.app").mkdir()
    return engine


def _find(*roots: Path, ue_root: Path | None = None) -> subprocess.CompletedProcess[str]:
    env = {k: v for k, v in os.environ.items() if k != "UE_ROOT"}
    env["RIFT_UE_SEARCH_ROOTS"] = ":".join(str(r) for r in roots)
    if ue_root is not None:
        env["UE_ROOT"] = str(ue_root)
    return subprocess.run(  # noqa: S603
        ["/bin/bash", str(SCRIPT)], env=env, capture_output=True, text=True, timeout=10
    )


def test_finds_engine_directly_under_root(tmp_path: Path) -> None:
    engine = _install(tmp_path, "UE_5.8")
    result = _find(tmp_path)
    assert result.returncode == 0
    assert result.stdout.strip() == str(engine)


def test_finds_engine_in_path_with_spaces(tmp_path: Path) -> None:
    root = tmp_path / "Epic Games"
    engine = _install(root, "UE_5.6")
    assert _find(tmp_path, root).stdout.strip() == str(engine)


def test_picks_newest_version_across_roots(tmp_path: Path) -> None:
    _install(tmp_path / "a", "UE_5.9")
    newest = _install(tmp_path / "b", "UE_5.10")
    _install(tmp_path / "a", "UE_5.4")
    assert _find(tmp_path / "a", tmp_path / "b").stdout.strip() == str(newest)


def test_ignores_install_without_editor(tmp_path: Path) -> None:
    _install(tmp_path, "UE_5.9", editor=False)
    engine = _install(tmp_path, "UE_5.7")
    assert _find(tmp_path).stdout.strip() == str(engine)


def test_nothing_found_exits_nonzero(tmp_path: Path) -> None:
    _install(tmp_path, "UE_5.8", editor=False)
    result = _find(tmp_path, tmp_path / "does-not-exist")
    assert result.returncode == 1
    assert result.stdout == ""


def test_ue_root_override(tmp_path: Path) -> None:
    _install(tmp_path / "search", "UE_5.9")
    custom = _install(tmp_path / "custom", "MyEngine")
    assert _find(tmp_path / "search", ue_root=custom).stdout.strip() == str(custom)
    # An explicit but invalid UE_ROOT is an error, not a silent fallback.
    assert _find(tmp_path / "search", ue_root=tmp_path / "nope").returncode == 1
