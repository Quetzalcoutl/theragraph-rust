-- Replaces the broad unreplayed_idx with a compound partial index that exactly
-- matches the replay-queue query in graph_dlq.rs:
--
--   SELECT ... FROM nebula_write_failures
--   WHERE replayed_at IS NULL
--     AND created_at > now() - interval '24 hours'
--     AND replay_count < 10
--   ORDER BY created_at ASC
--   LIMIT 50
--
-- The old index was (created_at DESC) WHERE replayed_at IS NULL, which:
--   - forced a backwards scan for ASC ordering
--   - did not filter replay_count < 10, leaving Postgres to recheck every row
--
-- Note: CONCURRENTLY is not used — SQLx migrations run inside a transaction
-- and Postgres forbids CONCURRENTLY inside a transaction block.
-- At startup the table is typically empty; the lock is instantaneous.

DROP INDEX IF EXISTS nebula_write_failures_unreplayed_idx;

CREATE INDEX IF NOT EXISTS nebula_write_failures_replay_queue_idx
    ON nebula_write_failures (created_at ASC)
    WHERE replayed_at IS NULL AND replay_count < 10;
