-- NASA-2: Nebula write failure DLQ.
--
-- Every best-effort Nebula write that fails (after the circuit breaker has
-- already fired or on a transport error) is recorded here so operators can:
--   1. Monitor failure rate: SELECT count(*) FROM nebula_write_failures WHERE created_at > now() - interval '1 hour'
--   2. Replay: replay_nebula_write_failures() stored proc or external script
--   3. Triage: group by operation_type to find the hot-spot
--
-- Replay strategy: the `query_preview` column holds the first 1000 chars of the
-- nGQL.  For idempotent edge writes (IF NOT EXISTS / UPSERT) re-sending the
-- original query is safe.  The reconciler already handles follow/like/purchase
-- misses automatically every 6h; this DLQ covers bookmark/view/comment writes
-- that the reconciler doesn't handle.

CREATE TABLE IF NOT EXISTS nebula_write_failures (
    id              BIGSERIAL PRIMARY KEY,
    operation_type  TEXT        NOT NULL,          -- e.g. "follows_edge", "likes_edge"
    user_address    TEXT,                          -- normalised lowercase Eth address
    post_id         TEXT,                          -- nft_id or UUID, when applicable
    query_preview   TEXT        NOT NULL,          -- first 1000 chars of the nGQL string
    error_message   TEXT        NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    replayed_at     TIMESTAMPTZ,
    replay_count    INT         NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS nebula_write_failures_created_at_idx
    ON nebula_write_failures (created_at DESC);

CREATE INDEX IF NOT EXISTS nebula_write_failures_operation_type_idx
    ON nebula_write_failures (operation_type, created_at DESC);

CREATE INDEX IF NOT EXISTS nebula_write_failures_unreplayed_idx
    ON nebula_write_failures (created_at DESC)
    WHERE replayed_at IS NULL;
