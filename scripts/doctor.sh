#!/usr/bin/env bash
# System doctor: toolchain + Unreal/Xcode status, then the canon (provider) doctor.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"

row() { printf '%-24s%-12s%s\n' "$1" "$2" "${3:-}"; }
check() { # label, command...
  local label="$1"; shift
  local out
  if out="$("$@" 2>/dev/null | head -1)" && [ -n "$out" ]; then row "$label" PASS "$out"; else row "$label" MISSING; fi
}

echo "Rift doctor (system)"
echo
row "OS" PASS "$(sw_vers -productName 2>/dev/null || uname -s) $(sw_vers -productVersion 2>/dev/null || uname -r) $(uname -m)"
check "Git" git --version
check "rustc" rustc --version
check "cargo" cargo --version
check "uv" uv --version
check "Python 3.12" uv run --project "$ROOT/canon" python --version

if xcodebuild -version >/dev/null 2>&1; then
  row "Xcode" PASS "$(xcodebuild -version | head -1)"
else
  row "Xcode" MISSING "only Command Line Tools; Unreal needs full Xcode (see game/README.md)"
fi

ue=$(ls -d "/Users/Shared/Epic Games"/UE_5* 2>/dev/null | tail -1)
if [ -n "$ue" ]; then row "Unreal Engine" PASS "$ue"; else row "Unreal Engine" MISSING "see game/README.md"; fi
if ls "$ROOT"/game/*/*.uproject >/dev/null 2>&1; then row "Unreal project" PASS "$(ls "$ROOT"/game/*/*.uproject)"; else row "Unreal project" MISSING; fi

if [ -f "$ROOT/.env" ]; then
  if git -C "$ROOT" check-ignore -q .env; then row ".env" PASS "present, git-ignored"; else row ".env" FAIL "NOT ignored by git"; fi
else
  row ".env" WARN "missing; cp .env.example .env"
fi
echo
cd "$ROOT/canon" && uv run --quiet python -m rift_canon.doctor "$@"
