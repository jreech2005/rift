import json

from rift_canon import schema_export


def test_checked_in_schemas_match_the_models() -> None:
    documents = schema_export.schemas()
    assert sorted(documents) == ["canon_packet.schema.json", "world_bible.schema.json"]
    for file_name, schema in documents.items():
        path = schema_export.SCHEMA_DIR / file_name
        assert path.is_file(), "run: uv run python -m rift_canon.schema_export"
        assert path.read_text(encoding="utf-8") == schema_export.render(schema), (
            f"{file_name} is out of date; run: uv run python -m rift_canon.schema_export"
        )


def test_schemas_describe_the_contract() -> None:
    documents = schema_export.schemas()
    bible = documents["world_bible.schema.json"]
    assert bible["$id"] == "rift://schemas/universe/v1/world_bible"
    assert bible["properties"]["schema_version"]["const"] == 1
    for field in ("universe", "player_role", "starting_location", "opening_conflict", "provenance"):
        assert field in bible["required"]
    assert bible["$defs"]["Classification"]["enum"] == ["canon", "inferred", "generated"]
    assert bible["$defs"]["PlayerRole"]["properties"]["classification"]["const"] == "generated"

    packet = documents["canon_packet.schema.json"]
    assert packet["$id"] == "rift://schemas/universe/v1/canon_packet"
    assert set(packet["required"]) >= {"universe", "documents", "retrieved_at", "provenance"}
    json.dumps(documents)  # plain JSON all the way down
