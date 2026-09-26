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

/// Music listen weight — sits between VIEW (passive) and LIKE (active intent).
/// A confirmed 30s+ listen with pct_played signals genuine engagement without
/// requiring the user to tap the like button. Slightly below LIKE_WEIGHT (1.0)
/// because listen intent is weaker than explicit like intent.
/// pct_played quality multiplier applied separately in write_music_creator_affinity.
pub const LISTEN_WEIGHT: f32 = 0.6;

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
mod tests;
