-- Stores blockchain interactions where the NFT UUID lookup returned None at event time.
-- A periodic repair job re-processes these once the NFT has been indexed by the Elixir app.
CREATE TABLE IF NOT EXISTS pending_interactions (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_address      TEXT        NOT NULL,
    contract_address  TEXT        NOT NULL,
    contract_type     TEXT        NOT NULL DEFAULT '',
    token_id          TEXT        NOT NULL,
    event_type        TEXT        NOT NULL,
    transaction_hash  TEXT        NOT NULL DEFAULT '',
    block_number      BIGINT      NOT NULL DEFAULT 0,
    retry_count       INT         NOT NULL DEFAULT 0,
    resolved_at       TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Deduplicate: same user/contract/token/event_type = same signal, keep first occurrence
    CONSTRAINT pending_interactions_dedup UNIQUE (user_address, contract_address, token_id, event_type)
);

CREATE INDEX IF NOT EXISTS pending_interactions_unresolved_idx
    ON pending_interactions (created_at)
    WHERE resolved_at IS NULL;
