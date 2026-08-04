-- TAG-S27-08: covering index for the update_trending_scores GROUP BY.
--
-- update_trending_scores previously used a correlated subquery:
--   SELECT SUM(...) FROM user_interactions WHERE nft_id = f.nft_id AND created_at > ...
-- Running once per row in nft_features caused a full sequential scan of user_interactions
-- on every hourly update cycle and held a write lock on nft_features for the full duration.
--
-- The rewritten CTE form does a single GROUP BY over user_interactions filtered to the
-- 7-day window. Without this index that GROUP BY itself does a sequential scan; with it
-- Postgres can use an index-only scan for both the WHERE and the GROUP BY, reducing the
-- trending update from O(N*M) to O(M log M) where M = interactions in the past 7 days.
-- CONCURRENTLY removed: migrations run inside a transaction block; CONCURRENTLY is
-- illegal there. A regular CREATE INDEX is safe here — migrations run at startup
-- before traffic reaches the table. Use CONCURRENTLY only for live hotfix scripts.
CREATE INDEX IF NOT EXISTS user_interactions_nft_id_created_at_idx
    ON user_interactions (nft_id, created_at DESC);
