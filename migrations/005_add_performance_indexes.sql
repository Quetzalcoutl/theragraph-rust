-- Performance indexes for slow-query hotspots observed in production logs.
--
-- recommendation_cache: SELECT WHERE user_address=$1 AND feed_type=$2 AND expires_at>NOW()
-- was taking 3-4s despite idx_recommendation_cache_user covering (user_address, feed_type).
-- Adding expires_at as the third column turns this into an index-only scan for non-expired
-- rows and lets Postgres prune expired entries at the index level before any heap fetch.
--
-- indexer_state: UPSERT ON CONFLICT (contract_address, contract_type) was slow because the
-- table has only a type-only index for reads; the UNIQUE constraint index handles conflict
-- detection but a covering index on both columns + last_block avoids the heap fetch on
-- the subsequent SELECT for the block number comparison.

-- Covering index for the hot cache-lookup query
CREATE INDEX IF NOT EXISTS idx_recommendation_cache_lookup
  ON recommendation_cache(user_address, feed_type, expires_at DESC);

-- Covering index for indexer_state lookups (contract polling hot path)
CREATE INDEX IF NOT EXISTS idx_indexer_state_lookup
  ON indexer_state(contract_address, contract_type, last_block);
