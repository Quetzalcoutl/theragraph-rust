-- Create indexer_state table for tracking last indexed blocks
-- Uses IF NOT EXISTS so Elixir migrations can own creation; this just ensures
-- the table exists with the correct composite unique constraint.
CREATE TABLE IF NOT EXISTS indexer_state (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_address VARCHAR(66) NOT NULL,
    contract_type VARCHAR(50) NOT NULL DEFAULT 'elixir_friends',
    last_block BIGINT NOT NULL,
    inserted_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(contract_address, contract_type)
);

-- Create index for faster lookups
CREATE INDEX IF NOT EXISTS idx_indexer_state_type ON indexer_state(contract_type);
