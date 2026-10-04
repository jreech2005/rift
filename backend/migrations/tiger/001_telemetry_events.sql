-- Rift gameplay telemetry: one row per event, partitioned by time.
-- Tiger Data / TimescaleDB. Safe to run repeatedly.
-- Statements are run one at a time, split on semicolons: keep semicolons out
-- of comments and string literals.

CREATE EXTENSION IF NOT EXISTS timescaledb;

CREATE TABLE IF NOT EXISTS rift_telemetry_events (
    ts            TIMESTAMPTZ      NOT NULL,
    session_id    UUID             NOT NULL,
    event_type    TEXT             NOT NULL,
    actor_id      TEXT,
    target_id     TEXT,
    location      TEXT,
    numeric_value DOUBLE PRECISION,
    metadata      JSONB            NOT NULL DEFAULT '{}'::jsonb
);

SELECT create_hypertable('rift_telemetry_events', 'ts', if_not_exists => TRUE);

CREATE INDEX IF NOT EXISTS rift_telemetry_events_session_ts
    ON rift_telemetry_events (session_id, ts DESC);
