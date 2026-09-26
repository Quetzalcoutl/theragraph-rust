-- apply_interaction_to_prefs / interaction_weight (recommendation/model/mod.rs)
-- weight Listen/FlixWatch interactions by event.pct_played, but user_interactions
-- never persisted it — insert_interaction (recorder/mod.rs) had no column to
-- bind it to. This makes a from-scratch replay of user_interactions through
-- apply_interaction_to_prefs (the rebuild/insurance-policy job) unable to
-- reproduce the exact original weight for those two interaction types; it
-- would have to fall back to full weight (unwrap_or(1.0)), silently
-- over-weighting historical listens/watches relative to what was actually
-- applied at the time.

ALTER TABLE user_interactions ADD COLUMN IF NOT EXISTS pct_played REAL;
