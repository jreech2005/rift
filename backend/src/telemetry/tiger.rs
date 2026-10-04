//! Tiger Data (TimescaleDB / PostgreSQL) telemetry.
//!
//! * always compiled — [`TigerConfig`] (env handling, secret redaction), the
//!   schema and the queries. Fully unit-tested without a database.
//! * `--features tiger` — [`TigerTelemetry`], the sink and reader.
//!
//! The schema lives in `backend/migrations/tiger/`. See `docs/TELEMETRY.md`.

use std::fmt;

use crate::director::Secret;

/// The one setting: a `postgres://` connection URL. It contains the password,
/// so it is a secret as a whole.
pub const TIGER_ENV_VAR: &str = "TIGER_DATABASE_URL";

pub const EVENTS_TABLE: &str = "rift_telemetry_events";
pub const MINUTE_VIEW: &str = "rift_telemetry_minute";

/// Hypertable and index. Required.
pub const SCHEMA_SQL: &str = include_str!("../../migrations/tiger/001_telemetry_events.sql");
/// Continuous aggregate. Optional: the backend works without it.
pub const AGGREGATE_SQL: &str = include_str!("../../migrations/tiger/002_telemetry_minute.sql");

pub const INSERT_SQL: &str = "INSERT INTO rift_telemetry_events \
    (ts, session_id, event_type, actor_id, target_id, location, numeric_value, metadata) \
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)";

/// The window counts of one session, in [`super::RecentCounts`] field order.
/// Reads the raw hypertable, so it does not need the continuous aggregate.
pub const RECENT_SQL: &str = "SELECT \
    count(*) FILTER (WHERE event_type = 'npc_interaction'), \
    count(DISTINCT location) FILTER (WHERE event_type = 'location_entered'), \
    count(*) FILTER (WHERE event_type = 'player_damaged'), \
    count(*) FILTER (WHERE event_type = 'player_died'), \
    count(*) FILTER (WHERE event_type = 'enemy_killed') \
    FROM rift_telemetry_events \
    WHERE session_id = $1 AND ts > $2 AND ts <= $3";

pub const DELETE_SESSION_SQL: &str = "DELETE FROM rift_telemetry_events WHERE session_id = $1";

/// Why there is no [`TigerConfig`]. Never contains the value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TigerConfigError {
    #[error("{TIGER_ENV_VAR} is not set")]
    Missing,
    #[error("{TIGER_ENV_VAR} must be a postgres:// or postgresql:// URL")]
    Invalid,
}

#[derive(Clone, PartialEq, Eq)]
pub struct TigerConfig {
    url: Secret,
    password: Option<Secret>,
}

impl fmt::Debug for TigerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TigerConfig")
            .field("url", &self.url)
            .finish()
    }
}

impl TigerConfig {
    pub fn from_env() -> Result<Self, TigerConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Build from any key lookup. A blank value counts as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, TigerConfigError> {
        let url = lookup(TIGER_ENV_VAR)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .ok_or(TigerConfigError::Missing)?;
        let rest = url
            .strip_prefix("postgres://")
            .or_else(|| url.strip_prefix("postgresql://"))
            .filter(|rest| !rest.is_empty())
            .ok_or(TigerConfigError::Invalid)?;
        let password = url_password(rest).map(Secret::new);
        Ok(Self {
            url: Secret::new(url),
            password,
        })
    }

    /// The connection URL. Only for opening the connection.
    pub fn url(&self) -> &Secret {
        &self.url
    }

    /// `message` with the URL and its password removed. Driver errors pass
    /// through this before they are logged or returned.
    pub fn redact(&self, message: &str) -> String {
        let message = self.url.redact(message);
        match &self.password {
            Some(password) => password.redact(&message),
            None => message,
        }
    }
}

/// The password of `user:password@host/...`, as written in the URL.
fn url_password(rest: &str) -> Option<&str> {
    let authority = rest.split(['/', '?']).next()?;
    let (userinfo, _host) = authority.rsplit_once('@')?;
    let (_user, password) = userinfo.split_once(':')?;
    (!password.is_empty()).then_some(password)
}

/// The statements of a migration file: comment lines dropped, split on `;`.
pub fn statements(sql: &str) -> Vec<String> {
    let body: String = sql
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    body.split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(feature = "tiger")]
pub use client::TigerTelemetry;

#[cfg(feature = "tiger")]
mod client {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use tokio::sync::{Mutex, mpsc};
    use tokio_postgres::Client;
    use tracing::{debug, info, warn};
    use uuid::Uuid;

    use super::{
        AGGREGATE_SQL, DELETE_SESSION_SQL, INSERT_SQL, RECENT_SQL, SCHEMA_SQL, TigerConfig,
        statements,
    };
    use crate::director::PlayerTelemetry;
    use crate::telemetry::{
        RecentCounts, TelemetryError, TelemetryEvent, TelemetryFuture, TelemetryReader,
        TelemetrySink, WINDOW_SECONDS, summarize,
    };

    /// Events waiting to be written. When full, new events are dropped.
    pub const QUEUE_CAPACITY: usize = 1024;
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    const OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
    const SCHEMA_TIMEOUT: Duration = Duration::from_secs(15);

    struct Inner {
        config: TigerConfig,
        client: Mutex<Option<Arc<Client>>>,
    }

    /// Telemetry in Tiger Data. Cheap to clone; clones share the connection
    /// and the write queue.
    ///
    /// [`TelemetrySink::record`] only queues: a background task does the
    /// writing, one event at a time, each under a timeout. The connection is
    /// opened on first use and reopened after a failure.
    #[derive(Clone)]
    pub struct TigerTelemetry {
        inner: Arc<Inner>,
        queue: mpsc::Sender<TelemetryEvent>,
    }

    impl std::fmt::Debug for TigerTelemetry {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("TigerTelemetry").finish_non_exhaustive()
        }
    }

    impl TigerTelemetry {
        /// Starts the writer task, so this must be called inside a Tokio
        /// runtime. Does not connect.
        pub fn new(config: &TigerConfig) -> Self {
            let inner = Arc::new(Inner {
                config: config.clone(),
                client: Mutex::new(None),
            });
            let (queue, mut pending) = mpsc::channel::<TelemetryEvent>(QUEUE_CAPACITY);
            let writer = inner.clone();
            tokio::spawn(async move {
                // One warning per outage, not one per event.
                let mut failing = false;
                while let Some(event) = pending.recv().await {
                    match writer.write(&event).await {
                        Ok(()) if failing => {
                            failing = false;
                            info!("telemetry: Tiger Data writes recovered");
                        }
                        Ok(()) => {}
                        Err(err) if !failing => {
                            failing = true;
                            warn!(error = %err, "telemetry: Tiger Data write failed; dropping events until it recovers");
                        }
                        Err(err) => debug!(error = %err, "telemetry event dropped"),
                    }
                }
            });
            Self { inner, queue }
        }

        /// Create the hypertable (required), then the continuous aggregate
        /// (optional). Returns whether the aggregate is in place.
        pub async fn ensure_schema(&self) -> Result<bool, TelemetryError> {
            for statement in statements(SCHEMA_SQL) {
                self.inner.execute_ddl(&statement).await?;
            }
            for statement in statements(AGGREGATE_SQL) {
                if let Err(err) = self.inner.execute_ddl(&statement).await {
                    warn!(error = %err, "telemetry: continuous aggregate unavailable; recent summaries are unaffected");
                    return Ok(false);
                }
            }
            Ok(true)
        }

        /// Write one event now, bypassing the queue.
        pub async fn write(&self, event: &TelemetryEvent) -> Result<(), TelemetryError> {
            self.inner.write(event).await
        }

        /// Remove a session's events. For tests and cleanup.
        pub async fn delete_session(&self, session_id: Uuid) -> Result<u64, TelemetryError> {
            let client = self.inner.client().await?;
            self.inner
                .run(
                    OPERATION_TIMEOUT,
                    client.execute(DELETE_SESSION_SQL, &[&session_id]),
                )
                .await
        }

        /// Server version, as a connectivity check.
        pub async fn ping(&self) -> Result<String, TelemetryError> {
            let client = self.inner.client().await?;
            let row = self
                .inner
                .run(
                    OPERATION_TIMEOUT,
                    client.query_one("SHOW server_version", &[]),
                )
                .await?;
            row.try_get(0)
                .map_err(|err| TelemetryError::Backend(err.to_string()))
        }
    }

    impl Inner {
        /// The open connection, or a new one.
        async fn client(&self) -> Result<Arc<Client>, TelemetryError> {
            let mut slot = self.client.lock().await;
            if let Some(client) = slot.as_ref().filter(|client| !client.is_closed()) {
                return Ok(client.clone());
            }
            let tls = native_tls::TlsConnector::new()
                .map(postgres_native_tls::MakeTlsConnector::new)
                .map_err(|err| TelemetryError::Unavailable(err.to_string()))?;
            let (client, connection) = self
                .run(
                    CONNECT_TIMEOUT,
                    tokio_postgres::connect(self.config.url().expose(), tls),
                )
                .await?;
            tokio::spawn(async move {
                if connection.await.is_err() {
                    debug!("telemetry: Tiger Data connection closed");
                }
            });
            let client = Arc::new(client);
            *slot = Some(client.clone());
            Ok(client)
        }

        /// Run one database call under a timeout, with errors redacted.
        async fn run<T>(
            &self,
            limit: Duration,
            call: impl Future<Output = Result<T, tokio_postgres::Error>>,
        ) -> Result<T, TelemetryError> {
            match tokio::time::timeout(limit, call).await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(err)) => {
                    let message = self.config.redact(&err.to_string());
                    Err(if err.as_db_error().is_some() {
                        TelemetryError::Backend(message)
                    } else {
                        TelemetryError::Unavailable(message)
                    })
                }
                Err(_) => Err(TelemetryError::Unavailable(
                    "Tiger Data operation timed out".to_owned(),
                )),
            }
        }

        async fn execute_ddl(&self, statement: &str) -> Result<(), TelemetryError> {
            let client = self.client().await?;
            self.run(SCHEMA_TIMEOUT, client.batch_execute(statement))
                .await
        }

        async fn write(&self, event: &TelemetryEvent) -> Result<(), TelemetryError> {
            let client = self.client().await?;
            self.run(
                OPERATION_TIMEOUT,
                client.execute(
                    INSERT_SQL,
                    &[
                        &event.timestamp,
                        &event.session_id,
                        &event.kind.as_str(),
                        &event.actor_id,
                        &event.target_id,
                        &event.location,
                        &event.numeric_value,
                        &event.metadata,
                    ],
                ),
            )
            .await
            .map(|_| ())
        }

        async fn recent(
            &self,
            session_id: Uuid,
            now: DateTime<Utc>,
        ) -> Result<PlayerTelemetry, TelemetryError> {
            let since = now - chrono::Duration::seconds(i64::from(WINDOW_SECONDS));
            let client = self.client().await?;
            let row = self
                .run(
                    OPERATION_TIMEOUT,
                    client.query_one(RECENT_SQL, &[&session_id, &since, &now]),
                )
                .await?;
            let count = |index: usize| -> Result<u64, TelemetryError> {
                let value: i64 = row
                    .try_get(index)
                    .map_err(|err| TelemetryError::Backend(err.to_string()))?;
                Ok(u64::try_from(value).unwrap_or(0))
            };
            Ok(summarize(RecentCounts {
                npc_interactions: count(0)?,
                distinct_locations: count(1)?,
                damage_events: count(2)?,
                deaths: count(3)?,
                kills: count(4)?,
            }))
        }
    }

    impl TelemetrySink for TigerTelemetry {
        fn record(&self, event: TelemetryEvent) {
            // Full or closed: the event is dropped. Gameplay does not wait.
            if self.queue.try_send(event).is_err() {
                debug!("telemetry queue full; event dropped");
            }
        }
    }

    impl TelemetryReader for TigerTelemetry {
        fn recent<'a>(
            &'a self,
            session_id: Uuid,
            now: DateTime<Utc>,
        ) -> TelemetryFuture<'a, PlayerTelemetry> {
            Box::pin(self.inner.recent(session_id, now))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str =
        "postgres://tsdbadmin:s3cret-pw@abc123.tsdb.cloud.timescale.com:5432/tsdb?sslmode=require";

    fn lookup(value: Option<&'static str>) -> impl Fn(&str) -> Option<String> {
        move |key| (key == TIGER_ENV_VAR).then(|| value.map(str::to_owned))?
    }

    #[test]
    fn unset_or_blank_is_missing() {
        assert_eq!(
            TigerConfig::from_lookup(lookup(None)),
            Err(TigerConfigError::Missing)
        );
        assert_eq!(
            TigerConfig::from_lookup(lookup(Some("   "))),
            Err(TigerConfigError::Missing)
        );
    }

    #[test]
    fn only_postgres_urls_are_accepted() {
        for bad in [
            "mysql://user:hunter2@h/db",
            "abc123.tsdb.cloud:5432",
            "postgres://",
        ] {
            let err = TigerConfig::from_lookup(lookup(Some(bad))).unwrap_err();
            assert_eq!(err, TigerConfigError::Invalid);
            assert!(!err.to_string().contains("hunter2"));
        }
        for good in [URL, "postgresql://localhost/rift"] {
            assert!(TigerConfig::from_lookup(lookup(Some(good))).is_ok());
        }
    }

    #[test]
    fn the_url_never_appears_in_debug_or_redacted_errors() {
        let config = TigerConfig::from_lookup(lookup(Some(URL))).unwrap();
        assert_eq!(config.url().expose(), URL);

        let debug = format!("{config:?}");
        assert!(!debug.contains("s3cret-pw"));
        assert!(!debug.contains("tsdbadmin"));

        let redacted = config.redact(&format!(
            "could not connect to {URL}: bad password s3cret-pw"
        ));
        assert!(!redacted.contains("s3cret-pw"));
        assert!(!redacted.contains("abc123"));
        assert!(redacted.contains("could not connect"));
    }

    #[test]
    fn password_extraction() {
        assert_eq!(url_password("user:pw@host:5432/db"), Some("pw"));
        assert_eq!(url_password("user:p@ss@host/db"), Some("p@ss"));
        assert_eq!(url_password("user@host/db"), None);
        assert_eq!(url_password("host/db?x=a:b@c"), None);
    }

    #[test]
    fn schema_is_a_hypertable_with_the_documented_columns() {
        let schema = statements(SCHEMA_SQL);
        assert_eq!(schema.len(), 4);
        assert!(schema.iter().all(|s| !s.contains("--")));
        let table = &schema[1];
        assert!(table.starts_with(&format!("CREATE TABLE IF NOT EXISTS {EVENTS_TABLE}")));
        for column in [
            "ts ",
            "session_id",
            "event_type",
            "actor_id",
            "target_id",
            "location",
            "numeric_value",
            "metadata",
        ] {
            assert!(table.contains(column), "missing column {column}");
        }
        assert!(table.contains("TIMESTAMPTZ") && table.contains("JSONB"));
        assert!(schema[2].contains("create_hypertable"));
        assert!(schema[2].contains("if_not_exists => TRUE"));
    }

    #[test]
    fn aggregate_is_a_continuous_aggregate_over_the_events() {
        let aggregate = statements(AGGREGATE_SQL);
        assert_eq!(aggregate.len(), 2);
        assert!(aggregate[0].contains(MINUTE_VIEW));
        assert!(aggregate[0].contains("timescaledb.continuous"));
        assert!(aggregate[0].contains("time_bucket"));
        assert!(aggregate[0].contains(EVENTS_TABLE));
        assert!(aggregate[1].contains("add_continuous_aggregate_policy"));
    }

    #[test]
    fn queries_read_and_write_the_events_table_with_parameters_only() {
        for sql in [INSERT_SQL, RECENT_SQL, DELETE_SESSION_SQL] {
            assert!(sql.contains(EVENTS_TABLE));
            assert!(sql.contains("$1"));
        }
        // Every event kind the summary counts is spelled as the sink writes it.
        use crate::telemetry::TelemetryEventKind as Kind;
        for kind in [
            Kind::NpcInteraction,
            Kind::LocationEntered,
            Kind::PlayerDamaged,
            Kind::PlayerDied,
            Kind::EnemyKilled,
        ] {
            assert!(RECENT_SQL.contains(&format!("'{}'", kind.as_str())));
        }
    }
}
