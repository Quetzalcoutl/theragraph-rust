-- Failed log DLQ: stores process_log_static failures so no event is permanently
-- lost to a transient write error. A background replayer (same pattern as the
-- nebula_write_failures DLQ) retries each row up to 10 times at 10-minute intervals.
-- After 10 attempts an ERROR is emitted for manual triage.

CREATE TABLE IF NOT EXISTS failed_logs (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tx_hash         TEXT        NOT NULL,
    block_number    BIGINT      NOT NULL,
    log_index       INT         NOT NULL DEFAULT 0,
    contract_type   TEXT        NOT NULL,
    contract_address TEXT       NOT NULL,
    event_type      TEXT,
    raw_data        JSONB,
    error_message   TEXT        NOT NULL,
    retry_count     INT         NOT NULL DEFAULT 0,
    replayed_at     TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Unique constraint prevents duplicate DLQ rows for the same log
CREATE UNIQUE INDEX IF NOT EXISTS failed_logs_tx_hash_log_idx
    ON failed_logs (tx_hash, log_index);

-- For the replayer: select rows needing retry, newest first
CREATE INDEX IF NOT EXISTS failed_logs_pending_idx
    ON failed_logs (created_at DESC)
    WHERE replayed_at IS NULL AND retry_count < 10;
