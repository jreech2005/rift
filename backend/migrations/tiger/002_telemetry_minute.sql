-- Optional: per-minute event counts per session, kept current by Timescale.
-- For dashboards and longer-range questions. The backend's recent summary
-- reads the raw hypertable and does not depend on this view.
-- Statements are run one at a time, split on semicolons, outside a
-- transaction (a continuous aggregate cannot be created inside one).

CREATE MATERIALIZED VIEW IF NOT EXISTS rift_telemetry_minute
WITH (timescaledb.continuous) AS
SELECT
    time_bucket(INTERVAL '1 minute', ts) AS bucket,
    session_id,
    event_type,
    count(*)                             AS events,
    sum(numeric_value)                   AS total_value
FROM rift_telemetry_events
GROUP BY bucket, session_id, event_type
WITH NO DATA;

SELECT add_continuous_aggregate_policy(
    'rift_telemetry_minute',
    start_offset      => INTERVAL '1 hour',
    end_offset        => INTERVAL '1 minute',
    schedule_interval => INTERVAL '1 minute',
    if_not_exists     => TRUE
);
