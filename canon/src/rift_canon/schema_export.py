"""Export the JSON Schemas of CanonPacket V1 and WorldBible V1.

    uv run python -m rift_canon.schema_export

The Pydantic models are the source of truth; ``shared/schemas/universe/v1/`` is
generated from them and a test fails if the two drift apart.
"""

from __future__ import annotations

import json
import sys
from typing import Any

from pydantic import BaseModel

from rift_canon.canon_packet import CanonPacket
from rift_canon.config import REPO_ROOT
from rift_canon.world_bible import WorldBible

SCHEMA_DIR = REPO_ROOT / "shared" / "schemas" / "universe" / "v1"
_MODELS: dict[str, type[BaseModel]] = {"canon_packet": CanonPacket, "world_bible": WorldBible}


def schemas() -> dict[str, dict[str, Any]]:
    """File name -> schema document."""
    documents = {}
    for name, model in _MODELS.items():
        source = f"canon/src/rift_canon/{name}.py"
        documents[f"{name}.schema.json"] = {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": f"rift://schemas/universe/v1/{name}",
            "$comment": f"Generated from {source} by rift_canon.schema_export. Do not edit.",
            **model.model_json_schema(),
        }
    return documents


def render(schema: dict[str, Any]) -> str:
    return json.dumps(schema, indent=2) + "\n"


def main() -> int:
    SCHEMA_DIR.mkdir(parents=True, exist_ok=True)
    for file_name, schema in schemas().items():
        path = SCHEMA_DIR / file_name
        path.write_text(render(schema), encoding="utf-8")
        print(path.relative_to(REPO_ROOT))
    return 0


if __name__ == "__main__":
    sys.exit(main())
