#!/usr/bin/env bash
# Print the newest installed Unreal Engine 5 root; exit 1 if none is found.
# Filesystem checks only: never launches or builds Unreal.
#
#   UE_ROOT               use exactly this engine root
#   RIFT_UE_SEARCH_ROOTS  colon-separated directories to search for UE_5* (default: Epic macOS layouts)
set -uo pipefail

EDITOR_APP="Engine/Binaries/Mac/UnrealEditor.app"
is_engine() { [ -d "$1/$EDITOR_APP" ]; }

if [ -n "${UE_ROOT:-}" ]; then
  if is_engine "$UE_ROOT"; then echo "${UE_ROOT%/}"; exit 0; fi
  exit 1
fi

ROOTS="${RIFT_UE_SEARCH_ROOTS:-/Users/Shared:/Users/Shared/Epic Games:/Applications:/Applications/Epic Games}"
found=""
IFS=':' read -r -a roots <<<"$ROOTS"
for root in "${roots[@]}"; do
  for dir in "$root"/UE_5*; do
    # Prefix with the directory name so sort -V orders by version, not by parent path.
    if is_engine "$dir"; then found+="$(basename "$dir")"$'\t'"$dir"$'\n'; fi
  done
done

[ -n "$found" ] || exit 1
printf '%s' "$found" | sort -V | tail -1 | cut -f2
