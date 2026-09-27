-- Cursor for the Elixir-purchases → recommendation signal follower
-- (src/event_processor/purchase_signals.rs). Single row (id = 1); the
-- (inserted_at, id) pair orders Elixir `purchases` rows totally.
CREATE TABLE IF NOT EXISTS purchase_signal_cursor (
    id SMALLINT PRIMARY KEY CHECK (id = 1),
    last_inserted_at TIMESTAMP NOT NULL,
    last_purchase_id UUID NOT NULL,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
