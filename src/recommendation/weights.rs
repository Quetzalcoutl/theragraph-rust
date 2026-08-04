/// Recommendation signal weights — single source of truth.
///
/// All layers that compute preference deltas (Rust engine, Elixir NIF,
/// any future pipeline) MUST use these values. Changing a weight here
/// changes it everywhere; duplicating it here means it can drift.
///
/// Tuning guide:
/// - PURCHASE_WEIGHT >> LIKE_WEIGHT: buying reveals strong preference
/// - LONG_VIEW vs VIEW: sustained attention ~= mild interest
/// - UNLIKE/UNSAVE are negative signals but weaker than positive ones
///   (user saw the item before disliking — recency dampens the signal)

/// Positive engagement weights
pub const LIKE_WEIGHT: f32 = 1.0;
pub const COMMENT_WEIGHT: f32 = LIKE_WEIGHT * 0.8;
pub const PURCHASE_WEIGHT: f32 = 3.0;
pub const SHARE_WEIGHT: f32 = LIKE_WEIGHT * 0.5;
pub const SAVE_WEIGHT: f32 = LIKE_WEIGHT * 0.7;

/// View weights (split by duration)
pub const VIEW_WEIGHT: f32 = 0.1;
pub const LONG_VIEW_WEIGHT: f32 = 0.3;

/// Duration threshold separating short from long views (milliseconds)
pub const LONG_VIEW_THRESHOLD_MS: i64 = 5000;

/// Negative signal weights
pub const UNLIKE_WEIGHT: f32 = -0.5;
pub const UNSAVE_WEIGHT: f32 = UNLIKE_WEIGHT * 0.5;
/// "Not interested" is the strongest negative signal — stronger than Unlike
/// because it is a deliberate explicit rejection rather than a reaction flip.
/// Applied to content_type affinity, tag_preferences, and creator_preferences.
pub const NOT_INTERESTED_WEIGHT: f32 = -1.5;

/// Daily decay applied to content-type affinities.
///
/// Formula: `new = 0.5 + (old - 0.5) * DECAY_FACTOR`
/// At 0.95/day, a score 0.3 above baseline decays to ~0.22 after a week.
pub const DECAY_FACTOR: f32 = 0.95;

/// Multipliers for affinity and tag updates (keep small for stability)
pub const AFFINITY_DELTA_FACTOR: f32 = 0.05;
pub const TAG_DELTA_FACTOR: f32 = 0.1;
pub const CREATOR_DELTA_FACTOR: f32 = 0.1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purchase_weight_greater_than_like_weight() {
        assert!(PURCHASE_WEIGHT > LIKE_WEIGHT);
    }

    #[test]
    fn like_weight_greater_than_comment_weight() {
        assert!(LIKE_WEIGHT > COMMENT_WEIGHT);
    }

    #[test]
    fn comment_weight_greater_than_save_weight() {
        assert!(COMMENT_WEIGHT > SAVE_WEIGHT);
    }

    #[test]
    fn save_weight_greater_than_share_weight() {
        assert!(SAVE_WEIGHT > SHARE_WEIGHT);
    }

    #[test]
    fn unlike_weight_is_negative() {
        assert!(UNLIKE_WEIGHT < 0.0);
    }

    #[test]
    fn unsave_weight_is_negative() {
        assert!(UNSAVE_WEIGHT < 0.0);
    }

    #[test]
    fn unlike_weight_more_negative_than_unsave_weight() {
        assert!(UNLIKE_WEIGHT < UNSAVE_WEIGHT);
    }

    #[test]
    fn view_weight_less_than_long_view_weight() {
        assert!(VIEW_WEIGHT < LONG_VIEW_WEIGHT);
    }

    #[test]
    fn decay_factor_in_range() {
        assert!(DECAY_FACTOR > 0.0 && DECAY_FACTOR < 1.0);
    }

    #[test]
    fn long_view_threshold_ms_positive() {
        assert!(LONG_VIEW_THRESHOLD_MS > 0);
    }

    #[test]
    fn all_delta_factors_in_range() {
        assert!(AFFINITY_DELTA_FACTOR > 0.0 && AFFINITY_DELTA_FACTOR < 1.0);
        assert!(TAG_DELTA_FACTOR > 0.0 && TAG_DELTA_FACTOR < 1.0);
        assert!(CREATOR_DELTA_FACTOR > 0.0 && CREATOR_DELTA_FACTOR < 1.0);
    }
}
