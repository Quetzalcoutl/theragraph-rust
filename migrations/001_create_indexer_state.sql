-- Create indexer_state table for tracking last indexed blocks
DROP TABLE IF EXISTS indexer_state;

CREATE TABLE indexer_state (
    id UUID PRIMARY KEY,
    contract_address VARCHAR(66) NOT NULL UNIQUE,
    contract_type VARCHAR(50) NOT NULL,
    last_block BIGINT NOT NULL,
    inserted_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Create index for faster lookups
CREATE INDEX IF NOT EXISTS idx_indexer_state_type ON indexer_state(contract_type);
