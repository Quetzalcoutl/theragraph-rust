//! Pure domain model for user preferences.
//!
//! All types, constants, and functions here are free of I/O: no `sqlx`, no
//! Redis, no async.  Safe to unit-test without a live database or cache.
//!
//! Async database and cache functions live in [`super::recorder`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::types::ContentType;

// ── Interaction types ─────────────────────────────────────────────────────────

/// Interaction types we track
// RS-13: Copy makes all-unit-variant enum copies zero-cost; eliminates explicit
// .clone() calls in api.rs where the value is used after being moved into InteractionEvent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionType {
    View,
    Like,
    Unlike,
    Comment,
    Purchase,
    Share,
    Save,
    Unsave,
    /// Explicit "not interested" / "see less of this" signal.
    /// Stronger negative weight than Unlike — this is intentional rejection,
    /// not just a reaction flip. Applied to content type, tags, and creator.
    NotInterested,
}

impl std::fmt::Display for InteractionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InteractionType::View => write!(f, "view"),
            InteractionType::Like => write!(f, "like"),
            InteractionType::Unlike => write!(f, "unlike"),
            InteractionType::Comment => write!(f, "comment"),
            InteractionType::Purchase => write!(f, "purchase"),
            InteractionType::Share => write!(f, "share"),
            InteractionType::Save => write!(f, "save"),
            InteractionType::Unsave => write!(f, "unsave"),
            InteractionType::NotInterested => write!(f, "not_interested"),
        }
    }
}

// ── User preference profile ───────────────────────────────────────────────────

/// User preference profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    pub user_address: String,

    // Content type affinities (0.0 to 1.0)
    pub snap_affinity: f32,
    pub art_affinity: f32,
    pub music_affinity: f32,
    pub flix_affinity: f32,

    // Tag preferences: tag -> weight
    pub tag_preferences: HashMap<String, f32>,

    // Creator preferences: address -> weight
    pub creator_preferences: HashMap<String, f32>,

    // Behavioral stats
    pub total_likes: i32,
    pub total_purchases: i32,
    pub total_views: i32,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            user_address: String::new(),
            snap_affinity: 0.5,
            art_affinity: 0.5,
            music_affinity: 0.5,
            flix_affinity: 0.5,
            tag_preferences: HashMap::new(),
            creator_preferences: HashMap::new(),
            total_likes: 0,
            total_purchases: 0,
            total_views: 0,
        }
    }
}

impl UserPreferences {
    /// Return the affinity score for the given content type.
    ///
    /// Single owner of the ContentType → affinity-field mapping so scoring.rs
    /// never needs a 4-arm match that must grow with every new content type.
    pub fn affinity_for(&self, ct: &ContentType) -> f32 {
        match ct {
            ContentType::Snap  => self.snap_affinity,
            ContentType::Art   => self.art_affinity,
            ContentType::Music => self.music_affinity,
            ContentType::Flix  => self.flix_affinity,
        }
    }
}

impl std::str::FromStr for InteractionType {
    type Err = ();
    /// Parse the snake_case string representation (e.g. `"not_interested"`).
    /// Returns `Err(())` for unknown values so the caller can return 400.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "view"           => Ok(Self::View),
            "like"           => Ok(Self::Like),
            "unlike"         => Ok(Self::Unlike),
            "purchase"       => Ok(Self::Purchase),
            "share"          => Ok(Self::Share),
            "save"           => Ok(Self::Save),
            "comment"        => Ok(Self::Comment),
            "unsave"         => Ok(Self::Unsave),
            "not_interested" => Ok(Self::NotInterested),
            _                => Err(()),
        }
    }
}

// ── Interaction event ─────────────────────────────────────────────────────────

/// Interaction event for recording
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionEvent {
    pub user_address: String,
    pub nft_id: String,
    pub interaction_type: InteractionType,
    pub view_duration_ms: Option<i64>,
    pub source: Option<String>,
    pub nft_contract_type: Option<String>,
    pub nft_creator_address: Option<String>,
    pub nft_tags: Vec<String>,
    /// Whether tags were available at record time. `Degraded` events can be
    /// re-enriched by a background repair job once the NFT is indexed.
    #[serde(default)]
    pub tag_enrichment: TagEnrichmentStatus,
    /// Caller-supplied idempotency key. When provided, the INSERT uses
    /// ON CONFLICT (event_id) DO NOTHING so replayed Kafka messages or
    /// retried API calls cannot insert duplicate interaction rows.
    /// Omit (None) for fire-and-forget paths that don't need dedup.
    #[serde(default)]
    pub event_id: Option<String>,
}

// ── Constants ─────────────────────────────────────────────────────────────────

/// Hard caps on preference map sizes.
pub const MAX_TAG_PREFS: usize = 200;
pub const MAX_CREATOR_PREFS: usize = 100;

// ── Eviction policy ───────────────────────────────────────────────────────────

/// Determines which entry to remove when a preference map exceeds its cap.
///
/// Pluggable so callers can choose the eviction strategy that matches their
/// domain semantics without editing the preference mutation code.
#[derive(Debug, Clone, Copy, Default)]
pub enum EvictionPolicy {
    /// Remove the entry with the lowest cumulative weight (default).
    /// Keeps the strongest signal; discards weakly-reinforced entries.
    #[default]
    LowestWeight,
}

// ── Tag enrichment status ─────────────────────────────────────────────────────

/// Tracks whether tag metadata was available when an interaction was recorded.
///
/// Interactions recorded while the NFT is not yet indexed arrive without tags.
/// Marking them `Degraded` allows a repair job to re-enrich them later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TagEnrichmentStatus {
    Complete,
    Degraded { reason: String },
}

impl Default for TagEnrichmentStatus {
    fn default() -> Self { Self::Complete }
}

// ── Weight re-exports ─────────────────────────────────────────────────────────

/// Preference learning weights — re-exported from [`super::weights`] for backward compat.
/// Import from `weights` directly in new code.
pub use super::weights::{
    LIKE_WEIGHT, PURCHASE_WEIGHT, VIEW_WEIGHT, LONG_VIEW_WEIGHT,
    UNLIKE_WEIGHT, LONG_VIEW_THRESHOLD_MS,
};
use super::weights::{
    COMMENT_WEIGHT, SHARE_WEIGHT, SAVE_WEIGHT, UNSAVE_WEIGHT, NOT_INTERESTED_WEIGHT,
    AFFINITY_DELTA_FACTOR, TAG_DELTA_FACTOR, CREATOR_DELTA_FACTOR,
};

// ── Pure domain functions ─────────────────────────────────────────────────────

/// Pure: returns the recommendation signal weight for an interaction.
/// No I/O — safe to call and test without a database.
pub fn interaction_weight(event: &InteractionEvent) -> f32 {
    match event.interaction_type {
        InteractionType::Like     => LIKE_WEIGHT,
        InteractionType::Comment  => COMMENT_WEIGHT,
        InteractionType::Purchase => PURCHASE_WEIGHT,
        InteractionType::View => {
            if event.view_duration_ms.unwrap_or(0) > LONG_VIEW_THRESHOLD_MS {
                LONG_VIEW_WEIGHT
            } else {
                VIEW_WEIGHT
            }
        }
        InteractionType::Unlike        => UNLIKE_WEIGHT,
        InteractionType::Unsave        => UNSAVE_WEIGHT,
        InteractionType::Share         => SHARE_WEIGHT,
        InteractionType::Save          => SAVE_WEIGHT,
        InteractionType::NotInterested => NOT_INTERESTED_WEIGHT,
    }
}

/// Pure: mutate `prefs` in-place to reflect one interaction.
/// No I/O — safe to call and test without a database.
pub fn apply_interaction_to_prefs(prefs: &mut UserPreferences, event: &InteractionEvent, policy: EvictionPolicy) {
    let weight = interaction_weight(event);

    if let Some(ref ct) = event.nft_contract_type {
        update_content_affinity(prefs, ct, weight);
    }

    for tag in &event.nft_tags {
        // EFF-004: only run O(n) eviction scan when a new key would push the map
        // over capacity. For existing tags (the common engaged-user path) the scan
        // is skipped entirely. TG-02 ordering is preserved: evict fires before insert.
        let current = prefs.tag_preferences.get(tag.as_str()).copied();
        if current.is_none() && prefs.tag_preferences.len() >= MAX_TAG_PREFS {
            evict(&mut prefs.tag_preferences, MAX_TAG_PREFS, policy);
        }
        prefs.tag_preferences.insert(
            tag.clone(),
            (current.unwrap_or(0.5) + weight * TAG_DELTA_FACTOR).clamp(0.0, 1.0),
        );
    }

    if let Some(ref creator) = event.nft_creator_address {
        let creator_lower = creator.to_lowercase();
        // EFF-004: same lazy-eviction pattern as tags — skip the O(n) scan for updates.
        let current = prefs.creator_preferences.get(creator_lower.as_str()).copied();
        if current.is_none() && prefs.creator_preferences.len() >= MAX_CREATOR_PREFS {
            evict(&mut prefs.creator_preferences, MAX_CREATOR_PREFS, policy);
        }
        prefs.creator_preferences.insert(
            creator_lower,
            (current.unwrap_or(0.5) + weight * CREATOR_DELTA_FACTOR).clamp(0.0, 1.0),
        );
    }

    match event.interaction_type {
        InteractionType::Like | InteractionType::Comment => prefs.total_likes = prefs.total_likes.saturating_add(1),
        InteractionType::Purchase => prefs.total_purchases = prefs.total_purchases.saturating_add(1),
        InteractionType::View     => prefs.total_views = prefs.total_views.saturating_add(1),
        // NotInterested: apply the negative weight (done above via interaction_weight)
        // but also cap the creator preference at 0.1 so this creator is
        // heavily suppressed without being zeroed (user might still interact later).
        InteractionType::NotInterested => {
            if let Some(ref creator) = event.nft_creator_address {
                let creator_lower = creator.to_lowercase();
                let current = prefs.creator_preferences.get(&creator_lower).copied().unwrap_or(0.5);
                // Force to 10% of current value, minimum 0.05 — aggressive suppression.
                let suppressed = (current * 0.1_f32).max(0.05_f32);
                prefs.creator_preferences.insert(creator_lower, suppressed);
            }
        }
        _ => {}
    }
}

/// Evict one entry from `map` when it exceeds `max_size`, using `policy`.
/// Pure — no allocations beyond the map mutation itself.
pub fn evict(map: &mut HashMap<String, f32>, max_size: usize, policy: EvictionPolicy) {
    if map.len() <= max_size { return; }
    match policy {
        EvictionPolicy::LowestWeight => {
            if let Some(key) = map
                .iter()
                .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(k, _)| k.clone())
            {
                map.remove(&key);
            }
        }
    }
}

/// Pure: update the content-type affinity field matching `contract_type`.
fn update_content_affinity(prefs: &mut UserPreferences, contract_type: &str, weight: f32) {
    let delta = weight * AFFINITY_DELTA_FACTOR;

    let affinity_field = match ContentType::from_str(contract_type) {
        Some(ContentType::Snap)  => &mut prefs.snap_affinity,
        Some(ContentType::Art)   => &mut prefs.art_affinity,
        Some(ContentType::Music) => &mut prefs.music_affinity,
        Some(ContentType::Flix)  => &mut prefs.flix_affinity,
        None                     => return,
    };
    *affinity_field = (*affinity_field + delta).clamp(0.0, 1.0);
}

/// Merge one onboarding preset into `prefs`.
/// Only writes to keys still at neutral (≤ 0.5) — never overwrites behavioral data.
///
/// `pub(crate)` so `recorder::seed_from_presets` can call it without exposing
/// it in the external public API.
pub(crate) fn apply_preset_seeds(prefs: &mut UserPreferences, preset_id: &str) {
    let tag_seeds: &[(&str, f32)] = match preset_id {
        "art_lover" => &[
            ("abstract", 0.85), ("surreal", 0.80), ("expressionism", 0.75),
            ("digital_art", 0.75), ("portrait", 0.70), ("fine_art", 0.70),
        ],
        "music_fan" => &[
            ("electronic", 0.85), ("hiphop", 0.80), ("jazz", 0.75),
            ("ambient", 0.70), ("beats", 0.70), ("indie", 0.65),
        ],
        "movie_buff" => &[
            ("cinematic", 0.85), ("short_film", 0.80), ("documentary", 0.75),
            ("animation", 0.75), ("experimental_film", 0.65),
        ],
        "snap_creator" => &[
            ("photography", 0.85), ("street_photography", 0.80),
            ("portrait", 0.75), ("nature", 0.70), ("urban", 0.65),
        ],
        "collector" => &[
            ("rare", 0.85), ("limited_edition", 0.82), ("exclusive", 0.80),
            ("generative", 0.75), ("1of1", 0.75),
        ],
        _ => return,
    };

    for (tag, seed) in tag_seeds {
        let current = prefs.tag_preferences.get(*tag).copied().unwrap_or(0.5);
        if current <= 0.5 {
            prefs.tag_preferences.insert(tag.to_string(), *seed);
        }
    }

    // Bump the matching content-type affinity if still at default.
    match preset_id {
        "art_lover"    => { if prefs.art_affinity   <= 0.5 { prefs.art_affinity   = 0.75; } }
        "music_fan"    => { if prefs.music_affinity <= 0.5 { prefs.music_affinity = 0.75; } }
        "movie_buff"   => { if prefs.flix_affinity  <= 0.5 { prefs.flix_affinity  = 0.75; } }
        "snap_creator" => { if prefs.snap_affinity  <= 0.5 { prefs.snap_affinity  = 0.75; } }
        _ => {}
    }
}

// ── Tests (pure function coverage) ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // Weight constants are pub-used at module level for LIKE/PURCHASE/VIEW/UNLIKE.
    // AFFINITY_DELTA_FACTOR is private in model.rs (not pub-used), so import directly.
    use super::super::weights::{LIKE_WEIGHT, PURCHASE_WEIGHT, VIEW_WEIGHT, UNLIKE_WEIGHT};
    use super::super::weights::AFFINITY_DELTA_FACTOR;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a minimal InteractionEvent for use in unit tests.
    fn make_event(
        interaction_type: InteractionType,
        contract_type: Option<&str>,
        creator: Option<&str>,
        tags: Vec<&str>,
        view_duration_ms: Option<i64>,
    ) -> InteractionEvent {
        InteractionEvent {
            user_address: "0xtest".to_string(),
            nft_id: "1".to_string(),
            interaction_type,
            view_duration_ms,
            source: None,
            nft_contract_type: contract_type.map(str::to_string),
            nft_creator_address: creator.map(str::to_string),
            nft_tags: tags.into_iter().map(str::to_string).collect(),
            tag_enrichment: TagEnrichmentStatus::Complete,
            event_id: None,
        }
    }

    // -----------------------------------------------------------------------
    // ContentType::from_str round-trips for all 4 variants
    // -----------------------------------------------------------------------

    #[test]
    fn content_type_from_str_roundtrip_snap() {
        let ct = ContentType::from_str("snap").expect("snap should parse");
        assert_eq!(ct, ContentType::Snap);
        assert_eq!(ct.as_str(), "snap");
    }

    #[test]
    fn content_type_from_str_roundtrip_art() {
        let ct = ContentType::from_str("art").expect("art should parse");
        assert_eq!(ct, ContentType::Art);
        assert_eq!(ct.as_str(), "art");
    }

    #[test]
    fn content_type_from_str_roundtrip_music() {
        let ct = ContentType::from_str("music").expect("music should parse");
        assert_eq!(ct, ContentType::Music);
        assert_eq!(ct.as_str(), "music");
    }

    #[test]
    fn content_type_from_str_roundtrip_flix() {
        let ct = ContentType::from_str("flix").expect("flix should parse");
        assert_eq!(ct, ContentType::Flix);
        assert_eq!(ct.as_str(), "flix");
    }

    // -----------------------------------------------------------------------
    // get_weight_for_content_type — verify correct affinity fields are updated
    // by apply_interaction_to_prefs with a known interaction weight.
    // -----------------------------------------------------------------------

    #[test]
    fn like_on_snap_content_increases_snap_affinity() {
        let mut prefs = UserPreferences::default();
        let before = prefs.snap_affinity;

        let event = make_event(InteractionType::Like, Some("snap"), None, vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        let expected = (before + LIKE_WEIGHT * AFFINITY_DELTA_FACTOR).clamp(0.0, 1.0);
        assert!(
            (prefs.snap_affinity - expected).abs() < 1e-6,
            "snap_affinity should be {expected:.6}, got {:.6}",
            prefs.snap_affinity
        );
        // Other affinities must be untouched.
        assert_eq!(prefs.art_affinity,   0.5);
        assert_eq!(prefs.music_affinity, 0.5);
        assert_eq!(prefs.flix_affinity,  0.5);
    }

    #[test]
    fn purchase_on_art_content_increases_art_affinity() {
        let mut prefs = UserPreferences::default();
        let before = prefs.art_affinity;

        let event = make_event(InteractionType::Purchase, Some("art"), None, vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        let expected = (before + PURCHASE_WEIGHT * AFFINITY_DELTA_FACTOR).clamp(0.0, 1.0);
        assert!(
            (prefs.art_affinity - expected).abs() < 1e-6,
            "art_affinity should be {expected:.6}, got {:.6}",
            prefs.art_affinity
        );
        assert_eq!(prefs.snap_affinity,  0.5);
        assert_eq!(prefs.music_affinity, 0.5);
        assert_eq!(prefs.flix_affinity,  0.5);
    }

    #[test]
    fn like_on_music_content_increases_music_affinity() {
        let mut prefs = UserPreferences::default();
        let before = prefs.music_affinity;

        let event = make_event(InteractionType::Like, Some("music"), None, vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        let expected = (before + LIKE_WEIGHT * AFFINITY_DELTA_FACTOR).clamp(0.0, 1.0);
        assert!(
            (prefs.music_affinity - expected).abs() < 1e-6,
            "music_affinity should be {expected:.6}, got {:.6}",
            prefs.music_affinity
        );
    }

    #[test]
    fn view_on_flix_content_increases_flix_affinity() {
        let mut prefs = UserPreferences::default();
        let before = prefs.flix_affinity;

        let event = make_event(InteractionType::View, Some("flix"), None, vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        let expected = (before + VIEW_WEIGHT * AFFINITY_DELTA_FACTOR).clamp(0.0, 1.0);
        assert!(
            (prefs.flix_affinity - expected).abs() < 1e-6,
            "flix_affinity should be {expected:.6}, got {:.6}",
            prefs.flix_affinity
        );
    }

    // -----------------------------------------------------------------------
    // Unknown content type → affinity fields untouched (returns 0.5 default)
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_content_type_leaves_all_affinities_at_default() {
        let mut prefs = UserPreferences::default();

        let event = make_event(InteractionType::Like, Some("video"), None, vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        assert_eq!(prefs.snap_affinity,  0.5, "snap_affinity should remain at default 0.5");
        assert_eq!(prefs.art_affinity,   0.5, "art_affinity should remain at default 0.5");
        assert_eq!(prefs.music_affinity, 0.5, "music_affinity should remain at default 0.5");
        assert_eq!(prefs.flix_affinity,  0.5, "flix_affinity should remain at default 0.5");
    }

    #[test]
    fn empty_content_type_string_leaves_affinities_unchanged() {
        let mut prefs = UserPreferences::default();

        let event = make_event(InteractionType::Like, Some(""), None, vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        assert_eq!(prefs.snap_affinity,  0.5);
        assert_eq!(prefs.art_affinity,   0.5);
        assert_eq!(prefs.music_affinity, 0.5);
        assert_eq!(prefs.flix_affinity,  0.5);
    }

    #[test]
    fn none_content_type_leaves_affinities_unchanged() {
        let mut prefs = UserPreferences::default();

        // nft_contract_type = None — update_content_affinity is never called.
        let event = make_event(InteractionType::Like, None, None, vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        assert_eq!(prefs.snap_affinity,  0.5);
        assert_eq!(prefs.art_affinity,   0.5);
        assert_eq!(prefs.music_affinity, 0.5);
        assert_eq!(prefs.flix_affinity,  0.5);
    }

    // -----------------------------------------------------------------------
    // Preference values clamped to [0.0, 1.0]
    // -----------------------------------------------------------------------

    #[test]
    fn affinity_does_not_exceed_1_0_after_many_positive_interactions() {
        let mut prefs = UserPreferences::default();
        prefs.snap_affinity = 0.99; // Start near the ceiling.

        // 50 likes on snap — without clamping this would overflow well past 1.0.
        for _ in 0..50 {
            let event = make_event(InteractionType::Like, Some("snap"), None, vec![], None);
            apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());
        }

        assert!(
            prefs.snap_affinity <= 1.0,
            "snap_affinity must not exceed 1.0; got {}",
            prefs.snap_affinity
        );
    }

    #[test]
    fn affinity_does_not_go_below_0_0_after_many_negative_interactions() {
        let mut prefs = UserPreferences::default();
        prefs.art_affinity = 0.01; // Start near the floor.

        // 50 unlikes on art — without clamping this would go negative.
        for _ in 0..50 {
            let event = make_event(InteractionType::Unlike, Some("art"), None, vec![], None);
            apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());
        }

        assert!(
            prefs.art_affinity >= 0.0,
            "art_affinity must not go below 0.0; got {}",
            prefs.art_affinity
        );
    }

    #[test]
    fn tag_preference_is_clamped_to_1_0_after_many_positive_interactions() {
        let mut prefs = UserPreferences::default();
        let tag = "photography".to_string();
        prefs.tag_preferences.insert(tag.clone(), 0.99);

        for _ in 0..50 {
            let event = make_event(InteractionType::Like, None, None, vec!["photography"], None);
            apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());
        }

        let value = prefs.tag_preferences.get(&tag).copied().unwrap_or(0.5);
        assert!(
            value <= 1.0,
            "tag preference must not exceed 1.0; got {value}"
        );
    }

    #[test]
    fn tag_preference_is_clamped_to_0_0_after_many_negative_interactions() {
        let mut prefs = UserPreferences::default();
        let tag = "abstract".to_string();
        prefs.tag_preferences.insert(tag.clone(), 0.01);

        for _ in 0..50 {
            let event = make_event(InteractionType::Unlike, None, None, vec!["abstract"], None);
            apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());
        }

        let value = prefs.tag_preferences.get(&tag).copied().unwrap_or(0.5);
        assert!(
            value >= 0.0,
            "tag preference must not go below 0.0; got {value}"
        );
    }

    // -----------------------------------------------------------------------
    // interaction_weight returns correct weights for each type
    // -----------------------------------------------------------------------

    #[test]
    fn interaction_weight_like() {
        let event = make_event(InteractionType::Like, None, None, vec![], None);
        assert_eq!(interaction_weight(&event), LIKE_WEIGHT);
    }

    #[test]
    fn interaction_weight_purchase() {
        let event = make_event(InteractionType::Purchase, None, None, vec![], None);
        assert_eq!(interaction_weight(&event), PURCHASE_WEIGHT);
    }

    #[test]
    fn interaction_weight_unlike_is_negative() {
        let event = make_event(InteractionType::Unlike, None, None, vec![], None);
        assert!(interaction_weight(&event) < 0.0, "unlike weight must be negative");
        assert_eq!(interaction_weight(&event), UNLIKE_WEIGHT);
    }

    #[test]
    fn interaction_weight_short_view() {
        let event = make_event(InteractionType::View, None, None, vec![], Some(100));
        assert_eq!(interaction_weight(&event), VIEW_WEIGHT);
    }

    // -----------------------------------------------------------------------
    // interaction_weight — additional cases from the required test matrix
    // -----------------------------------------------------------------------

    #[test]
    fn interaction_weight_long_view() {
        use super::super::weights::LONG_VIEW_WEIGHT;
        // Any duration strictly greater than LONG_VIEW_THRESHOLD_MS triggers the long-view branch.
        let event = make_event(
            InteractionType::View,
            None,
            None,
            vec![],
            Some(LONG_VIEW_THRESHOLD_MS + 1),
        );
        assert_eq!(interaction_weight(&event), LONG_VIEW_WEIGHT);
    }

    #[test]
    fn interaction_weight_view_none_duration() {
        // view_duration_ms = None  →  unwrap_or(0) = 0, which is NOT > threshold  →  VIEW_WEIGHT
        let event = make_event(InteractionType::View, None, None, vec![], None);
        assert_eq!(interaction_weight(&event), VIEW_WEIGHT);
    }

    // -----------------------------------------------------------------------
    // evict — standalone tests (test matrix items 6-8)
    // -----------------------------------------------------------------------

    #[test]
    fn evict_no_op_when_within_capacity() {
        let mut map: HashMap<String, f32> = HashMap::new();
        map.insert("a".to_string(), 0.1);
        map.insert("b".to_string(), 0.9);
        // max_size == map.len() → no eviction
        evict(&mut map, 2, EvictionPolicy::LowestWeight);
        assert_eq!(map.len(), 2, "map should be unchanged when len <= max_size");
        assert!(map.contains_key("a"));
        assert!(map.contains_key("b"));
    }

    #[test]
    fn evict_lowest_weight_removes_min_entry() {
        let mut map: HashMap<String, f32> = HashMap::new();
        map.insert("high".to_string(), 0.9);
        map.insert("low".to_string(), 0.1);
        map.insert("mid".to_string(), 0.5);
        // map.len() = 3 > max_size = 2 → should remove "low"
        evict(&mut map, 2, EvictionPolicy::LowestWeight);
        assert_eq!(map.len(), 2, "one entry should have been evicted");
        assert!(!map.contains_key("low"), "the lowest-weight entry must be removed");
        assert!(map.contains_key("high"));
        assert!(map.contains_key("mid"));
    }

    #[test]
    fn evict_max_size_zero_removes_sole_entry() {
        let mut map: HashMap<String, f32> = HashMap::new();
        map.insert("only".to_string(), 0.5);
        // max_size = 0 < map.len() = 1 → should evict the single entry
        evict(&mut map, 0, EvictionPolicy::LowestWeight);
        assert!(map.is_empty(), "map should be empty after evicting the only entry");
    }

    // -----------------------------------------------------------------------
    // apply_interaction_to_prefs — tag_preferences and counter tests
    // (test matrix items 9-13)
    // -----------------------------------------------------------------------

    #[test]
    fn like_on_tagged_nft_increases_tag_preference() {
        use super::super::weights::TAG_DELTA_FACTOR;
        let mut prefs = UserPreferences::default();
        let tag = "landscape";
        let before = prefs.tag_preferences.get(tag).copied().unwrap_or(0.5);

        let event = make_event(InteractionType::Like, None, None, vec![tag], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        let after = prefs.tag_preferences.get(tag).copied().expect("tag should exist");
        let expected = (before + LIKE_WEIGHT * TAG_DELTA_FACTOR).clamp(0.0, 1.0);
        assert!(
            (after - expected).abs() < 1e-6,
            "tag_preferences[{tag}] should be {expected:.6}, got {after:.6}"
        );
        assert!(after > before, "tag preference should have increased after a Like");
    }

    #[test]
    fn like_increments_total_likes() {
        let mut prefs = UserPreferences::default();
        assert_eq!(prefs.total_likes, 0);

        let event = make_event(InteractionType::Like, None, None, vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        assert_eq!(prefs.total_likes, 1, "total_likes should increment by 1 after a Like");
    }

    #[test]
    fn purchase_increments_total_purchases() {
        let mut prefs = UserPreferences::default();
        assert_eq!(prefs.total_purchases, 0);

        let event = make_event(InteractionType::Purchase, None, None, vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        assert_eq!(prefs.total_purchases, 1, "total_purchases should increment by 1 after a Purchase");
    }

    #[test]
    fn short_view_increments_total_views() {
        let mut prefs = UserPreferences::default();
        assert_eq!(prefs.total_views, 0);

        // Short view: duration = 100 ms (well below LONG_VIEW_THRESHOLD_MS)
        let event = make_event(InteractionType::View, None, None, vec![], Some(100));
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        assert_eq!(prefs.total_views, 1, "total_views should increment by 1 after a View");
    }

    #[test]
    fn unlike_decreases_tag_preference() {
        use super::super::weights::TAG_DELTA_FACTOR;
        let mut prefs = UserPreferences::default();
        let tag = "portrait";
        // Start at the default uninitialised value (0.5).
        let before = 0.5_f32;
        prefs.tag_preferences.insert(tag.to_string(), before);

        let event = make_event(InteractionType::Unlike, None, None, vec![tag], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

        let after = prefs.tag_preferences.get(tag).copied().expect("tag should still exist");
        let expected = (before + UNLIKE_WEIGHT * TAG_DELTA_FACTOR).clamp(0.0, 1.0);
        assert!(
            (after - expected).abs() < 1e-6,
            "tag_preferences[{tag}] should be {expected:.6}, got {after:.6}"
        );
        assert!(after < before, "tag preference should have decreased after an Unlike");
    }
}
