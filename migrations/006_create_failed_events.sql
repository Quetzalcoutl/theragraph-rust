-- DLQ table for commit-and-skipped poison-pill Kafka events.
-- Moved from inline DDL in event_processor::run() (DLQ-S23-01) so schema
-- is managed by sqlx migrations and not reconstructed on every process start.
CREATE TABLE IF NOT EXISTS failed_events (
    id BIGSERIAL PRIMARY KEY,
    failed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    topic TEXT NOT NULL,
    partition INTEGER NOT NULL,
    "offset" BIGINT NOT NULL,
    payload TEXT,
    error_type TEXT NOT NULL,
    error_message TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS failed_events_failed_at
    ON failed_events (failed_at DESC);
