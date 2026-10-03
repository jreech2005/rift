# rift-canon

Pre-game canon/data pipeline for Rift. Phase 0 provides configuration, provider interfaces and `doctor`.

```sh
uv run python -m rift_canon.doctor          # config check, no network
uv run python -m rift_canon.doctor --live   # + free read-only connectivity checks
uv run pytest
```
