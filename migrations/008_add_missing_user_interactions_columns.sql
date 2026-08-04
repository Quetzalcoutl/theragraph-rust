-- TAG-S28-01: event_id was referenced in recorder.rs INSERT and ON CONFLICT clause
--   but never added to the user_interactions table in migration 002.
--   Without this column every insert_interaction() call fails at runtime.
--
-- TAG-S28-02: block_number was referenced in interaction.rs:445 by is_stale_like_event()
--   to guard against out-of-order Kafka redelivery (Like block N after Unlike block N+1).
--   Without this column the guard silently returns false (unwrap_or(0)) and the
--   Nebula edge state can diverge from on-chain truth.

ALTER TABLE user_interactions ADD COLUMN IF NOT EXISTS event_id UUID;
ALTER TABLE user_interactions ADD COLUMN IF NOT EXISTS block_number BIGINT;

-- Partial unique index: NULL event_id = fire-and-forget path that does not
-- dedup, so NULLs are excluded. The recorder.rs ON CONFLICT (event_id) DO NOTHING
-- clause relies on this index for idempotent Kafka redelivery protection.
CREATE UNIQUE INDEX IF NOT EXISTS user_interactions_event_id_uidx
    ON user_interactions (event_id)
    WHERE event_id IS NOT NULL;
