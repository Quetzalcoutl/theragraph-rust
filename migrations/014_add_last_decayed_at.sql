-- apply_preference_decay (recorder/decay.rs) gated its UPDATE on
-- last_activity_at < NOW() - INTERVAL '1 day', but last_activity_at is
-- refreshed by ANY interaction (recorder/mod.rs save_preferences) — so:
--   - an active user's preferences NEVER decay (the predicate never matches
--     while they keep interacting, even about unrelated tags)
--   - a dormant user's preferences decay every hour the score-updater cron
--     runs (engagement_update_interval, default 3600s) instead of once per
--     day, since the predicate stays true on every tick until they return —
--     0.95^24 ≈ 0.29, wiping ~71% of preference deviation in a single day
--     instead of the gradual week-scale fade documented in
--     weights::DECAY_FACTOR and RECOMMENDATION_SYSTEM.md.
--
-- last_decayed_at tracks decay application independently of activity, so the
-- gate becomes "has it been ≥1 day since we last decayed this row", not
-- "has it been ≥1 day since any interaction".

ALTER TABLE user_preferences ADD COLUMN IF NOT EXISTS last_decayed_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP;

CREATE INDEX IF NOT EXISTS user_preferences_last_decayed_at_idx ON user_preferences (last_decayed_at);
