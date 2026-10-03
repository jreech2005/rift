"""TiDB — persistence for canon data, outside the gameplay latency path.

Phase 0 has no SQL driver; the live check only opens a TCP connection to
confirm the host is reachable. It does not authenticate.
"""

from __future__ import annotations

import asyncio

import httpx

from rift_canon.providers.base import LiveResult, Provider

CONNECT_TIMEOUT_S = 5.0


class TiDBProvider(Provider):
    name = "TiDB"
    env_vars = ("TIDB_HOST", "TIDB_PORT", "TIDB_USER", "TIDB_PASSWORD", "TIDB_DATABASE")

    def is_configured(self) -> bool:
        s = self.settings
        return all(
            v is not None
            for v in (s.tidb_host, s.tidb_port, s.tidb_user, s.tidb_password, s.tidb_database)
        )

    async def live_check(self, client: httpx.AsyncClient) -> LiveResult:
        assert self.settings.tidb_host is not None and self.settings.tidb_port is not None
        _, writer = await asyncio.wait_for(
            asyncio.open_connection(self.settings.tidb_host, self.settings.tidb_port),
            CONNECT_TIMEOUT_S,
        )
        writer.close()
        await writer.wait_closed()
        return LiveResult(True, "TCP reachable (auth not tested)")
