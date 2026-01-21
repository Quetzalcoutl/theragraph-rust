-- Add feed_type column to recommendation_cache when missing
-- This migration is safe to run on existing DBs that may have been created
-- before `feed_type` was introduced.
BEGIN;

ALTER TABLE recommendation_cache
  ADD COLUMN IF NOT EXISTS feed_type VARCHAR(20) NOT NULL DEFAULT 'following';

-- Add index for efficient lookups by user and feed
CREATE INDEX IF NOT EXISTS idx_recommendation_cache_user_feed
  ON recommendation_cache(user_address, feed_type);

COMMIT;