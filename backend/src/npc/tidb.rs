//! TiDB persistence for NPC memory and state.
//!
//! Split in two so the default build stays offline and dependency-free:
//!
//! * always compiled — [`TiDbConfig`] (env handling, secret redaction), table
//!   naming, DDL and row encoding. Fully unit-tested without a database.
//! * `--features tidb` — [`TiDbStore`], a [`MemoryStore`](super::store::MemoryStore)
//!   over the MySQL protocol (`mysql_async`, TLS via rustls).
//!
//! TiDB is never on the gameplay path: authoritative NPC state lives in
//! [`NpcDirectory`](super::NpcDirectory); this store is written to and queried
//! from async tasks only.
//!
//! # Retrieval and the vector/full-text extension point
//!
//! V1 ranking is done in Rust by [`rank_memories`](super::memory::rank_memories)
//! over a bounded candidate set (the NPC's newest and most important
//! memories), so TiDB and the in-memory store rank identically and no
//! embedding provider is needed. The table keeps `summary` as its own column
//! so semantic retrieval can be added without a migration of existing rows:
//!
//! ```sql
//! ALTER TABLE npc_memories ADD COLUMN embedding VECTOR(768) NULL;
//! ALTER TABLE npc_memories ADD VECTOR INDEX idx_embedding ((VEC_COSINE_DISTANCE(embedding)));
//! -- candidates: ... ORDER BY VEC_COSINE_DISTANCE(embedding, ?) LIMIT 50
//! -- or:         ALTER TABLE npc_memories ADD FULLTEXT INDEX idx_summary (summary);
//! ```
//!
//! Only the candidate query in `TiDbStore::query_relevant` changes; scoring,
//! bounds and the `MemoryStore` contract stay as they are.

use std::fmt;

use super::memory::MemoryEntry;
use super::state::CharacterState;
use super::store::StoreError;

pub const TIDB_ENV_VARS: [&str; 5] = [
    "TIDB_HOST",
    "TIDB_PORT",
    "TIDB_USER",
    "TIDB_PASSWORD",
    "TIDB_DATABASE",
];
/// Optional override (`true`/`false`). By default TLS is on for every host
/// except loopback; TiDB Cloud requires it.
pub const TIDB_TLS_ENV_VAR: &str = "TIDB_TLS";

/// Newest memories considered as candidates by `query_relevant`.
pub const RECENT_CANDIDATES: usize = 200;
/// Most important memories additionally considered as candidates.
pub const IMPORTANT_CANDIDATES: usize = 100;
/// Maximum size of one serialized memory or state row.
pub const MAX_ROW_BYTES: usize = 65_536;

const REDACTED: &str = "<redacted>";

/// A credential. Never printed, logged or serialized.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Only for handing to the database driver.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TiDbConfigError {
    /// Not configured. Carries the names (never values) of the unset variables.
    #[error("TiDB is not configured; missing {}", .0.join(", "))]
    Missing(Vec<&'static str>),
    #[error("TIDB_PORT is not a valid port number")]
    InvalidPort,
    #[error("TIDB_TLS must be `true` or `false`")]
    InvalidTls,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiDbConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Secret,
    pub database: String,
    pub tls: bool,
}

impl TiDbConfig {
    /// Read `TIDB_*` from the process environment.
    pub fn from_env() -> Result<Self, TiDbConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, TiDbConfigError> {
        let get = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let missing: Vec<&'static str> = TIDB_ENV_VARS
            .into_iter()
            .filter(|key| get(key).is_none())
            .collect();
        if !missing.is_empty() {
            return Err(TiDbConfigError::Missing(missing));
        }
        let require = |key: &str| get(key).unwrap_or_default();

        let host = require("TIDB_HOST");
        let port = require("TIDB_PORT")
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(TiDbConfigError::InvalidPort)?;
        let tls = match get(TIDB_TLS_ENV_VAR).map(|v| v.to_lowercase()).as_deref() {
            None => !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"),
            Some("true" | "1") => true,
            Some("false" | "0") => false,
            Some(_) => return Err(TiDbConfigError::InvalidTls),
        };
        Ok(Self {
            host,
            port,
            user: require("TIDB_USER"),
            // Passwords may legitimately start or end with spaces.
            password: Secret::new(lookup("TIDB_PASSWORD").unwrap_or_default()),
            database: require("TIDB_DATABASE"),
            tls,
        })
    }

    /// Remove the password from a message before it is logged or returned.
    pub fn redact(&self, message: &str) -> String {
        let password = self.password.expose();
        if password.is_empty() {
            message.to_owned()
        } else {
            message.replace(password, REDACTED)
        }
    }
}

/// Table names for one deployment. A prefix lets tests use disposable tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableNames {
    pub prefix: String,
    pub memories: String,
    pub states: String,
}

impl TableNames {
    /// `prefix` may be empty; otherwise at most 32 chars of `[a-z0-9_]`. It is
    /// interpolated into SQL, hence the strict charset.
    pub fn new(prefix: &str) -> Result<Self, StoreError> {
        if prefix.len() > 32
            || !prefix
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(StoreError::InvalidEntry(
                "table prefix must be at most 32 chars of [a-z0-9_]".into(),
            ));
        }
        Ok(Self {
            prefix: prefix.to_owned(),
            memories: format!("{prefix}npc_memories"),
            states: format!("{prefix}npc_character_states"),
        })
    }

    /// Idempotent DDL for both tables.
    pub fn create_statements(&self) -> [String; 2] {
        [
            format!(
                "CREATE TABLE IF NOT EXISTS `{}` (\
                 memory_id CHAR(36) NOT NULL, \
                 session_id CHAR(36) NOT NULL, \
                 character_id VARCHAR(64) NOT NULL, \
                 event_id CHAR(36) NULL, \
                 memory_type VARCHAR(32) NOT NULL, \
                 location VARCHAR(64) NULL, \
                 importance TINYINT UNSIGNED NOT NULL, \
                 emotional_valence TINYINT NOT NULL, \
                 world_time BIGINT UNSIGNED NOT NULL, \
                 created_at_us BIGINT NOT NULL, \
                 valid TINYINT(1) NOT NULL DEFAULT 1, \
                 summary VARCHAR(500) NOT NULL, \
                 entry MEDIUMTEXT NOT NULL, \
                 PRIMARY KEY (memory_id), \
                 KEY idx_owner_recent (session_id, character_id, valid, world_time), \
                 KEY idx_owner_importance (session_id, character_id, valid, importance))",
                self.memories
            ),
            format!(
                "CREATE TABLE IF NOT EXISTS `{}` (\
                 session_id CHAR(36) NOT NULL, \
                 character_id VARCHAR(64) NOT NULL, \
                 version BIGINT UNSIGNED NOT NULL, \
                 updated_at_us BIGINT NOT NULL, \
                 state MEDIUMTEXT NOT NULL, \
                 PRIMARY KEY (session_id, character_id))",
                self.states
            ),
        ]
    }

    /// DDL removing both tables. Refused for the unprefixed (real) tables.
    pub fn drop_statements(&self) -> Result<[String; 2], StoreError> {
        if self.prefix.is_empty() {
            return Err(StoreError::InvalidEntry(
                "refusing to drop unprefixed tables".into(),
            ));
        }
        Ok([
            format!("DROP TABLE IF EXISTS `{}`", self.memories),
            format!("DROP TABLE IF EXISTS `{}`", self.states),
        ])
    }
}

fn check_row_size(what: &str, json: String) -> Result<String, StoreError> {
    if json.len() > MAX_ROW_BYTES {
        return Err(StoreError::InvalidEntry(format!(
            "{what} exceeds {MAX_ROW_BYTES} bytes"
        )));
    }
    Ok(json)
}

/// Validate and serialize a memory for the `entry` column.
pub fn encode_memory(entry: &MemoryEntry) -> Result<String, StoreError> {
    entry
        .validate()
        .map_err(|e| StoreError::InvalidEntry(e.to_string()))?;
    let json = serde_json::to_string(entry).map_err(|e| StoreError::InvalidEntry(e.to_string()))?;
    check_row_size("memory entry", json)
}

/// Decode a row of the memories table. The `valid` column is authoritative
/// (invalidation updates only the column).
pub fn decode_memory(entry_json: &str, valid: bool) -> Result<MemoryEntry, StoreError> {
    let mut entry: MemoryEntry =
        serde_json::from_str(entry_json).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    entry.valid = valid;
    entry
        .validate()
        .map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok(entry)
}

/// Validate and serialize a character state for the `state` column.
pub fn encode_state(state: &CharacterState) -> Result<String, StoreError> {
    state
        .validate()
        .map_err(|e| StoreError::InvalidEntry(e.to_string()))?;
    let json = serde_json::to_string(state).map_err(|e| StoreError::InvalidEntry(e.to_string()))?;
    check_row_size("character state", json)
}

/// Decode a row of the character-states table.
pub fn decode_state(state_json: &str) -> Result<CharacterState, StoreError> {
    let state: CharacterState =
        serde_json::from_str(state_json).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    state
        .validate()
        .map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok(state)
}

#[cfg(feature = "tidb")]
pub use client::TiDbStore;

#[cfg(feature = "tidb")]
mod client {
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::time::Duration;

    use mysql_async::prelude::Queryable;
    use mysql_async::{OptsBuilder, Params, Pool, PoolConstraints, PoolOpts, SslOpts, Value};
    use uuid::Uuid;

    use super::*;
    use crate::npc::ids::CharacterId;
    use crate::npc::memory::{MemoryQuery, ScoredMemory, clamp_limit, rank_memories};
    use crate::npc::store::{MemoryStore, StoreFuture};

    /// Budget for one store operation, connection included.
    const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

    /// NPC memory and state in TiDB. Cheap to clone (shares one pool).
    #[derive(Debug, Clone)]
    pub struct TiDbStore {
        pool: Pool,
        tables: TableNames,
        config: TiDbConfig,
    }

    impl TiDbStore {
        /// A store on the real tables. Connects lazily, on first use.
        pub fn new(config: &TiDbConfig) -> Result<Self, StoreError> {
            Self::with_table_prefix(config, "")
        }

        /// A store whose tables are named `<prefix>npc_*`.
        pub fn with_table_prefix(config: &TiDbConfig, prefix: &str) -> Result<Self, StoreError> {
            let tables = TableNames::new(prefix)?;
            // Keep one connection warm: reconnecting costs a TLS handshake per call.
            let constraints = PoolConstraints::new(1, 4).expect("1 <= 4");
            let opts = OptsBuilder::default()
                .ip_or_hostname(config.host.clone())
                .tcp_port(config.port)
                .user(Some(config.user.clone()))
                .pass(Some(config.password.expose().to_owned()))
                .db_name(Some(config.database.clone()))
                .ssl_opts(config.tls.then(SslOpts::default))
                .pool_opts(PoolOpts::default().with_constraints(constraints));
            Ok(Self {
                pool: Pool::new(opts),
                tables,
                config: config.clone(),
            })
        }

        pub fn tables(&self) -> &TableNames {
            &self.tables
        }

        /// Run one database operation under the timeout, mapping errors to
        /// [`StoreError`] with the password scrubbed.
        async fn run<T>(
            &self,
            operation: impl Future<Output = Result<T, mysql_async::Error>>,
        ) -> Result<T, StoreError> {
            match tokio::time::timeout(OPERATION_TIMEOUT, operation).await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(mysql_async::Error::Server(e))) => {
                    Err(StoreError::Backend(self.config.redact(&e.to_string())))
                }
                Ok(Err(e)) => Err(StoreError::Unavailable(self.config.redact(&e.to_string()))),
                Err(_) => Err(StoreError::Unavailable("TiDB operation timed out".into())),
            }
        }

        /// Connect and return the server version string.
        pub async fn ping(&self) -> Result<String, StoreError> {
            let version: Option<String> = self
                .run(async {
                    let mut conn = self.pool.get_conn().await?;
                    conn.query_first("SELECT VERSION()").await
                })
                .await?;
            version.ok_or_else(|| StoreError::Corrupt("SELECT VERSION() returned no row".into()))
        }

        /// Create the tables if they do not exist.
        pub async fn ensure_schema(&self) -> Result<(), StoreError> {
            for statement in self.tables.create_statements() {
                self.run(async {
                    let mut conn = self.pool.get_conn().await?;
                    conn.query_drop(statement).await
                })
                .await?;
            }
            Ok(())
        }

        /// Drop this store's tables. Only allowed for prefixed (disposable)
        /// tables.
        pub async fn drop_tables(&self) -> Result<(), StoreError> {
            for statement in self.tables.drop_statements()? {
                self.run(async {
                    let mut conn = self.pool.get_conn().await?;
                    conn.query_drop(statement).await
                })
                .await?;
            }
            Ok(())
        }

        /// Persist a snapshot of an NPC's state (last write wins).
        pub async fn save_character_state(&self, state: &CharacterState) -> Result<(), StoreError> {
            let json = encode_state(state)?;
            let sql = format!(
                "INSERT INTO `{}` (session_id, character_id, version, updated_at_us, state) \
                 VALUES (?, ?, ?, ?, ?) \
                 ON DUPLICATE KEY UPDATE version = VALUES(version), \
                 updated_at_us = VALUES(updated_at_us), state = VALUES(state)",
                self.tables.states
            );
            let params = Params::Positional(vec![
                Value::from(state.session_id().to_string()),
                Value::from(state.character_id().as_str()),
                Value::from(state.version()),
                Value::from(state.updated_at().timestamp_micros()),
                Value::from(json),
            ]);
            self.run(async {
                let mut conn = self.pool.get_conn().await?;
                conn.exec_drop(sql, params).await
            })
            .await
        }

        /// Load the persisted snapshot of an NPC's state, if any.
        pub async fn load_character_state(
            &self,
            session_id: Uuid,
            character_id: &CharacterId,
        ) -> Result<Option<CharacterState>, StoreError> {
            let sql = format!(
                "SELECT state FROM `{}` WHERE session_id = ? AND character_id = ?",
                self.tables.states
            );
            let row: Option<String> = self
                .run(async {
                    let mut conn = self.pool.get_conn().await?;
                    conn.exec_first(sql, (session_id.to_string(), character_id.as_str()))
                        .await
                })
                .await?;
            row.as_deref().map(decode_state).transpose()
        }

        /// Close the pool.
        pub async fn disconnect(self) -> Result<(), StoreError> {
            let config = self.config.clone();
            self.pool
                .disconnect()
                .await
                .map_err(|e| StoreError::Unavailable(config.redact(&e.to_string())))
        }

        async fn select_memories(
            &self,
            session_id: Uuid,
            character_id: &CharacterId,
            order_by: &str,
            limit: usize,
        ) -> Result<Vec<MemoryEntry>, StoreError> {
            let sql = format!(
                "SELECT entry, valid FROM `{}` \
                 WHERE session_id = ? AND character_id = ? AND valid = 1 \
                 ORDER BY {order_by} LIMIT {limit}",
                self.tables.memories
            );
            let rows: Vec<(String, i64)> = self
                .run(async {
                    let mut conn = self.pool.get_conn().await?;
                    conn.exec(sql, (session_id.to_string(), character_id.as_str()))
                        .await
                })
                .await?;
            rows.iter()
                .map(|(json, valid)| decode_memory(json, *valid != 0))
                .collect()
        }
    }

    const RECENT_ORDER: &str = "world_time DESC, created_at_us DESC, memory_id ASC";
    const IMPORTANT_ORDER: &str = "importance DESC, world_time DESC, memory_id ASC";

    impl MemoryStore for TiDbStore {
        fn store_memory<'a>(&'a self, entry: &'a MemoryEntry) -> StoreFuture<'a, ()> {
            Box::pin(async move {
                let json = encode_memory(entry)?;
                let table = &self.tables.memories;
                let owner_sql =
                    format!("SELECT session_id, character_id FROM `{table}` WHERE memory_id = ?");
                let upsert_sql = format!(
                    "INSERT INTO `{table}` (memory_id, session_id, character_id, event_id, \
                     memory_type, location, importance, emotional_valence, world_time, \
                     created_at_us, valid, summary, entry) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                     ON DUPLICATE KEY UPDATE event_id = VALUES(event_id), \
                     memory_type = VALUES(memory_type), location = VALUES(location), \
                     importance = VALUES(importance), \
                     emotional_valence = VALUES(emotional_valence), \
                     world_time = VALUES(world_time), created_at_us = VALUES(created_at_us), \
                     valid = VALUES(valid), summary = VALUES(summary), entry = VALUES(entry)"
                );
                let memory_id = entry.memory_id.to_string();
                let session_id = entry.session_id.to_string();
                let params = Params::Positional(vec![
                    Value::from(memory_id.clone()),
                    Value::from(session_id.clone()),
                    Value::from(entry.character_id.as_str()),
                    Value::from(entry.event_id.map(|id| id.to_string())),
                    Value::from(entry.memory_type.as_str()),
                    Value::from(entry.location.as_ref().map(|l| l.as_str().to_owned())),
                    Value::from(entry.importance),
                    Value::from(entry.emotional_valence),
                    Value::from(entry.world_time),
                    Value::from(entry.created_at.timestamp_micros()),
                    Value::from(i8::from(entry.valid)),
                    Value::from(entry.summary.as_str()),
                    Value::from(json),
                ]);
                let stored = self
                    .run(async {
                        let mut conn = self.pool.get_conn().await?;
                        let owner: Option<(String, String)> =
                            conn.exec_first(owner_sql, (memory_id,)).await?;
                        if owner.is_some_and(|(sid, cid)| {
                            sid != session_id || cid != entry.character_id.as_str()
                        }) {
                            return Ok(false);
                        }
                        conn.exec_drop(upsert_sql, params).await?;
                        Ok(true)
                    })
                    .await?;
                if stored {
                    Ok(())
                } else {
                    Err(StoreError::Conflict(entry.memory_id))
                }
            })
        }

        fn get_memory(&self, memory_id: Uuid) -> StoreFuture<'_, Option<MemoryEntry>> {
            Box::pin(async move {
                let sql = format!(
                    "SELECT entry, valid FROM `{}` WHERE memory_id = ?",
                    self.tables.memories
                );
                let row: Option<(String, i64)> = self
                    .run(async {
                        let mut conn = self.pool.get_conn().await?;
                        conn.exec_first(sql, (memory_id.to_string(),)).await
                    })
                    .await?;
                row.map(|(json, valid)| decode_memory(&json, valid != 0))
                    .transpose()
            })
        }

        fn query_recent<'a>(
            &'a self,
            session_id: Uuid,
            character_id: &'a CharacterId,
            limit: usize,
        ) -> StoreFuture<'a, Vec<MemoryEntry>> {
            Box::pin(self.select_memories(
                session_id,
                character_id,
                RECENT_ORDER,
                clamp_limit(limit),
            ))
        }

        fn query_relevant<'a>(
            &'a self,
            query: &'a MemoryQuery,
        ) -> StoreFuture<'a, Vec<ScoredMemory>> {
            Box::pin(async move {
                // Extension point: replace these two candidate queries with a
                // vector / full-text search (see the module docs).
                let mut candidates: BTreeMap<Uuid, MemoryEntry> = BTreeMap::new();
                for (order_by, limit) in [
                    (RECENT_ORDER, RECENT_CANDIDATES),
                    (IMPORTANT_ORDER, IMPORTANT_CANDIDATES),
                ] {
                    let rows = self
                        .select_memories(query.session_id, &query.character_id, order_by, limit)
                        .await?;
                    candidates.extend(rows.into_iter().map(|m| (m.memory_id, m)));
                }
                Ok(rank_memories(candidates.into_values(), query))
            })
        }

        fn invalidate_memory(&self, memory_id: Uuid) -> StoreFuture<'_, bool> {
            Box::pin(async move {
                let sql = format!(
                    "UPDATE `{}` SET valid = 0 WHERE memory_id = ? AND valid = 1",
                    self.tables.memories
                );
                self.run(async {
                    let mut conn = self.pool.get_conn().await?;
                    conn.exec_drop(sql, (memory_id.to_string(),)).await?;
                    Ok(conn.affected_rows() > 0)
                })
                .await
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::npc::events::test_support::npc;
    use crate::npc::memory::test_support::memory;

    fn lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    const FULL: [(&str, &str); 5] = [
        ("TIDB_HOST", "gateway01.us-west-2.prod.aws.tidbcloud.com"),
        ("TIDB_PORT", "4000"),
        ("TIDB_USER", "abc123.root"),
        ("TIDB_PASSWORD", "s3cr3t-Passw0rd!"),
        ("TIDB_DATABASE", "rift"),
    ];

    #[test]
    fn absent_credentials_are_reported_by_name() {
        let err = TiDbConfig::from_lookup(|_| None).unwrap_err();
        assert_eq!(err, TiDbConfigError::Missing(TIDB_ENV_VARS.to_vec()));
        assert_eq!(
            err.to_string(),
            "TiDB is not configured; missing TIDB_HOST, TIDB_PORT, TIDB_USER, \
             TIDB_PASSWORD, TIDB_DATABASE"
        );

        // Empty and whitespace-only values (as in `.env.example`) count as unset.
        let partial = [
            ("TIDB_HOST", "db.example"),
            ("TIDB_PORT", "4000"),
            ("TIDB_USER", "  "),
            ("TIDB_PASSWORD", ""),
        ];
        assert_eq!(
            TiDbConfig::from_lookup(lookup(&partial)),
            Err(TiDbConfigError::Missing(vec![
                "TIDB_USER",
                "TIDB_PASSWORD",
                "TIDB_DATABASE"
            ]))
        );
    }

    #[test]
    fn reads_complete_configuration() {
        let cfg = TiDbConfig::from_lookup(lookup(&FULL)).unwrap();
        assert_eq!(cfg.host, "gateway01.us-west-2.prod.aws.tidbcloud.com");
        assert_eq!(cfg.port, 4000);
        assert_eq!(cfg.user, "abc123.root");
        assert_eq!(cfg.password.expose(), "s3cr3t-Passw0rd!");
        assert_eq!(cfg.database, "rift");
        assert!(cfg.tls, "remote hosts default to TLS");
    }

    #[test]
    fn rejects_invalid_port_and_tls() {
        for port in ["0", "70000", "four thousand", "-1"] {
            let mut vars = FULL.to_vec();
            vars[1] = ("TIDB_PORT", port);
            assert_eq!(
                TiDbConfig::from_lookup(lookup(&vars)),
                Err(TiDbConfigError::InvalidPort),
                "{port:?}"
            );
        }
        let mut vars = FULL.to_vec();
        vars.push(("TIDB_TLS", "maybe"));
        assert_eq!(
            TiDbConfig::from_lookup(lookup(&vars)),
            Err(TiDbConfigError::InvalidTls)
        );
    }

    #[test]
    fn tls_defaults_off_only_for_loopback_and_can_be_overridden() {
        let mut vars = FULL.to_vec();
        vars[0] = ("TIDB_HOST", "127.0.0.1");
        assert!(!TiDbConfig::from_lookup(lookup(&vars)).unwrap().tls);
        vars.push(("TIDB_TLS", "true"));
        assert!(TiDbConfig::from_lookup(lookup(&vars)).unwrap().tls);

        let mut vars = FULL.to_vec();
        vars.push(("TIDB_TLS", "FALSE"));
        assert!(!TiDbConfig::from_lookup(lookup(&vars)).unwrap().tls);
    }

    #[test]
    fn secrets_are_redacted() {
        let cfg = TiDbConfig::from_lookup(lookup(&FULL)).unwrap();
        let debug = format!("{cfg:?} {:?}", cfg.password);
        assert!(!debug.contains("s3cr3t"), "{debug}");
        assert!(debug.contains("<redacted>"));
        assert!(
            debug.contains("abc123.root"),
            "non-secret fields stay visible"
        );

        let message = "Access denied for 'abc123.root' (using password: s3cr3t-Passw0rd!)";
        let redacted = cfg.redact(message);
        assert!(!redacted.contains("s3cr3t"));
        assert_eq!(
            redacted,
            "Access denied for 'abc123.root' (using password: <redacted>)"
        );

        // Config errors only ever name variables.
        let mut vars = FULL.to_vec();
        vars[1] = ("TIDB_PORT", "nope");
        let err = TiDbConfig::from_lookup(lookup(&vars)).unwrap_err();
        assert!(!format!("{err} {err:?}").contains("s3cr3t"));
    }

    #[test]
    fn table_names_and_ddl() {
        let real = TableNames::new("").unwrap();
        assert_eq!(real.memories, "npc_memories");
        assert_eq!(real.states, "npc_character_states");
        assert!(
            real.drop_statements().is_err(),
            "real tables cannot be dropped"
        );
        let [memories, states] = real.create_statements();
        assert!(memories.starts_with("CREATE TABLE IF NOT EXISTS `npc_memories` ("));
        assert!(memories.contains("PRIMARY KEY (memory_id)"));
        assert!(states.starts_with("CREATE TABLE IF NOT EXISTS `npc_character_states` ("));

        let test = TableNames::new("rift_test_ab12_").unwrap();
        assert_eq!(test.memories, "rift_test_ab12_npc_memories");
        assert_eq!(
            test.drop_statements().unwrap(),
            [
                "DROP TABLE IF EXISTS `rift_test_ab12_npc_memories`",
                "DROP TABLE IF EXISTS `rift_test_ab12_npc_character_states`"
            ]
        );

        for bad in [
            "x`; DROP TABLE users; --",
            "Upper",
            "with space",
            &"a".repeat(33),
        ] {
            assert!(TableNames::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn memory_rows_round_trip_and_reject_corruption() {
        let m = memory("hank", 7, "The player threatened me.", 75);
        let json = encode_memory(&m).unwrap();
        assert_eq!(decode_memory(&json, true).unwrap(), m);
        // The `valid` column wins over the serialized flag.
        assert!(!decode_memory(&json, false).unwrap().valid);

        assert!(matches!(
            decode_memory("{not json", true),
            Err(StoreError::Corrupt(_))
        ));
        let tampered = json.replace("\"hank\"", "\"hank; --\"");
        assert!(matches!(
            decode_memory(&tampered, true),
            Err(StoreError::Corrupt(_))
        ));

        let mut invalid = m.clone();
        invalid.importance = 200;
        assert!(matches!(
            encode_memory(&invalid),
            Err(StoreError::InvalidEntry(_))
        ));
        let mut huge = m;
        huge.metadata
            .insert("blob".into(), serde_json::json!("x".repeat(MAX_ROW_BYTES)));
        assert!(matches!(
            encode_memory(&huge),
            Err(StoreError::InvalidEntry(_))
        ));
    }

    #[test]
    fn state_rows_round_trip_and_reject_corruption() {
        let state = npc("hank", "Hank Schrader", Some("dea_office"));
        let json = encode_state(&state).unwrap();
        assert_eq!(decode_state(&json).unwrap(), state);
        assert!(matches!(
            decode_state(&json.replace("\"schema_version\":1", "\"schema_version\":9")),
            Err(StoreError::Corrupt(_))
        ));
        assert!(matches!(decode_state("[]"), Err(StoreError::Corrupt(_))));
    }
}
