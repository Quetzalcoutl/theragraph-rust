-- Marketplace token ids are uint256 (ticket 037, wire contract .wayfinder/research/037-wire-contract.md).
--
-- The v3 market mints copy ids (originalId << 64) | serial, far past BIGINT. `nft_features.token_id` is
-- the only marketplace id column this service owns (the Elixir indexer owns `nfts` and never reads or
-- writes `nft_features`): widen it to NUMERIC(78,0), the size of a uint256 (78 decimal digits).
--
--   * the Rust service reads it as `token_id::text` and writes it as `$n::numeric` (src/token_id.rs),
--     so the same binary works on BIGINT (before this runs) and on NUMERIC(78,0) (after);
--   * every existing id converts exactly (BIGINT to NUMERIC loses nothing);
--   * `pending_interactions.token_id` stays TEXT on purpose: it is a dedup key that is only ever
--     compared for equality, and the service writes the canonical decimal string into it.
--
-- Idempotent: a second run (or a database that never had the table) changes nothing.
-- Rollback by hand, only while every stored id still fits a BIGINT (it errors otherwise, which is right):
--   ALTER TABLE nft_features ALTER COLUMN token_id TYPE BIGINT USING token_id::bigint;

DO $$
BEGIN
    IF to_regclass('public.nft_features') IS NOT NULL
       AND EXISTS (SELECT 1 FROM information_schema.columns
                   WHERE table_schema = 'public' AND table_name = 'nft_features'
                     AND column_name = 'token_id' AND data_type <> 'numeric') THEN
        ALTER TABLE nft_features ALTER COLUMN token_id TYPE NUMERIC(78,0) USING token_id::numeric;
    END IF;
END $$;
