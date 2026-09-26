//! Pure domain model for user preferences.
//!
//! All types, constants, and functions here are free of I/O: no `sqlx`, no
//! Redis, no async.  Safe to unit-test without a live database or cache.
//!
//! Async database and cache functions live in [`super::recorder`].

mod genesis;
pub use genesis::*;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::types::ContentType;

// ── Interaction types ─────────────────────────────────────────────────────────

/// Interaction types we track
// RS-13: Copy makes all-unit-variant enum copies zero-cost; eliminates explicit
// .clone() calls in api.rs where the value is used after being moved into InteractionEvent.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionType {
    View,
    /// Dedicated music listen signal — carries duration_ms + pct_played.
    /// Distinct from View (feed scroll) so Nebula can apply music-specific
    /// half-life (10 days) and write EDGE_MUSIC_LISTEN instead of view_event.
    /// 30-second minimum enforced at the API boundary before this variant fires.
    Listen,
    /// Dedicated flix watch signal — carries pct_played (completion rate) +
    /// view_duration_ms (actual watch seconds). Distinct from View so Nebula
    /// can apply flix-specific half-life and write EDGE_FLIX_WATCH.
    /// 10-second minimum enforced at the API boundary (Next.js route).
    FlixWatch,
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
            InteractionType::Listen => write!(f, "listen"),
            InteractionType::FlixWatch => write!(f, "flix_watch"),
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
    pub tag_preferences: HashMap<Box<str>, f32>,

    // Creator preferences: address -> weight
    pub creator_preferences: HashMap<Box<str>, f32>,

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
    /// ContentType is Copy — take by value, skip the redundant borrow.
    pub fn affinity_for(&self, ct: ContentType) -> f32 {
        match ct {
            ContentType::Snap  => self.snap_affinity,
            ContentType::Art   => self.art_affinity,
            ContentType::Music => self.music_affinity,
            ContentType::Flix  => self.flix_affinity,
        }
    }

    /// Tags this user has a strong (above tag-match-threshold) affinity for,
    /// sorted by weight descending. Uses the same threshold
    /// `scoring::compute`'s tag-match check applies (0.65 once the map
    /// exceeds 20 tags, else 0.6) so "strong affinity" means the same thing
    /// here as it does in scoring — used to retrieve candidates a user's
    /// affinity should be able to surface even outside the default recency
    /// window, not just re-rank what's already in it.
    pub fn top_tags(&self, limit: usize) -> Vec<String> {
        let threshold = if self.tag_preferences.len() > 20 { 0.65 } else { 0.6 };
        let mut tags: Vec<(&str, f32)> = self
            .tag_preferences
            .iter()
            .filter(|(_, &w)| w > threshold)
            .map(|(t, &w)| (t.as_ref(), w))
            .collect();
        tags.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        tags.truncate(limit);
        tags.into_iter().map(|(t, _)| t.to_string()).collect()
    }

    /// Creators this user has a strong affinity for, sorted by weight
    /// descending. Same purpose as `top_tags` — surfaces older/niche work by
    /// a favorite creator that the recency-only candidate pool would exclude.
    pub fn top_creators(&self, limit: usize) -> Vec<String> {
        let mut creators: Vec<(&str, f32)> = self
            .creator_preferences
            .iter()
            .filter(|(_, &w)| w > 0.6)
            .map(|(c, &w)| (c.as_ref(), w))
            .collect();
        creators.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        creators.truncate(limit);
        creators.into_iter().map(|(c, _)| c.to_string()).collect()
    }
}

impl std::str::FromStr for InteractionType {
    type Err = ();
    /// Parse the snake_case string representation (e.g. `"not_interested"`).
    /// Returns `Err(())` for unknown values so the caller can return 400.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "view"           => Ok(Self::View),
            "listen"         => Ok(Self::Listen),
            "flix_watch"     => Ok(Self::FlixWatch),
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
///
/// GENRE-01: previously carried a `genre_ids: Vec<i32>` field ("Genre IDs from
/// nft_genres at play time") that arrived at the API boundary, was cloned into
/// this struct, and then reached `dispatch_graph_interaction` as an
/// underscore-prefixed, never-read parameter — a complete no-op end to end.
/// Removed rather than wired up: genre now flows through `nft_tags` instead
/// (NFT-side genre is folded into `tags` as slugs at ingestion — see
/// `event_processor::elixir_db::process_enrichment`), and `nft_tags` already
/// feeds both the persisted `tag_preferences` learning
/// (`update_preferences_from_interaction`) and the session recency boost
/// (`SessionSignal.tags` in `record_interaction`) — a strictly better-wired
/// signal path than the dead numeric-id field ever was. A user's *declared*
/// (not listen-time) genre taste has its own dedicated write path —
/// `write_genre_preference_edges` / `POST /api/v1/genre-preferences/{addr}` —
/// so keeping a second, redundant way to write the same fact here was avoided.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionEvent {
    pub user_address: String,
    pub nft_id: String,
    pub interaction_type: InteractionType,
    pub view_duration_ms: Option<i64>,
    /// Music listen completion rate 0.0–1.0. Only set for Listen interactions.
    /// Spotify's primary listen quality signal: 0.95 = nearly full play, 0.12 = skip.
    #[serde(default)]
    pub pct_played: Option<f32>,
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
    LISTEN_WEIGHT, AFFINITY_DELTA_FACTOR, TAG_DELTA_FACTOR, CREATOR_DELTA_FACTOR,
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
        InteractionType::Listen => {
            LISTEN_WEIGHT * event.pct_played.unwrap_or(1.0).clamp(0.0, 1.0)
        }
        InteractionType::Unlike        => UNLIKE_WEIGHT,
        InteractionType::Unsave        => UNSAVE_WEIGHT,
        InteractionType::Share         => SHARE_WEIGHT,
        InteractionType::Save          => SAVE_WEIGHT,
        InteractionType::NotInterested => NOT_INTERESTED_WEIGHT,
        InteractionType::FlixWatch => {
            // YouTube engineers: weight = completion rate × base watch weight.
            // High-completion watch (0.9+) ≈ Like signal. Skip (0.1) ≈ mild View.
            LISTEN_WEIGHT * event.pct_played.unwrap_or(1.0).clamp(0.0, 1.0)
        }
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
        //
        // BUG-CAP-OFF-BY-ONE: evict()'s own contract (see its tests) is "trim
        // until len() <= target" — a no-op when already exactly at target. This
        // loop's trigger condition fires at len() == MAX_TAG_PREFS (not yet
        // over), so passing MAX_TAG_PREFS as evict's target made it a no-op
        // right when we needed it to free a slot, and the unconditional insert
        // right after grew the map to MAX_TAG_PREFS + 1 — the cap only ever
        // took effect one interaction later. Target MAX_TAG_PREFS - 1 instead:
        // evict() then actually removes one entry (len() > target), and the
        // insert that follows lands back on exactly MAX_TAG_PREFS.
        let current = prefs.tag_preferences.get(tag.as_str()).copied();
        if current.is_none() && prefs.tag_preferences.len() >= MAX_TAG_PREFS {
            evict(&mut prefs.tag_preferences, MAX_TAG_PREFS - 1, policy);
        }
        prefs.tag_preferences.insert(
            tag.as_str().into(),
            (current.unwrap_or(0.5) + weight * TAG_DELTA_FACTOR).clamp(0.0, 1.0),
        );
    }

    if let Some(ref creator) = event.nft_creator_address {
        let creator_lower = creator.to_lowercase();
        // EFF-004: same lazy-eviction pattern as tags — skip the O(n) scan for
        // updates. BUG-CAP-OFF-BY-ONE: same fix as the tag loop above.
        let current = prefs.creator_preferences.get(creator_lower.as_str()).copied();
        if current.is_none() && prefs.creator_preferences.len() >= MAX_CREATOR_PREFS {
            evict(&mut prefs.creator_preferences, MAX_CREATOR_PREFS - 1, policy);
        }
        prefs.creator_preferences.insert(
            creator_lower.into(),
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
                let current = prefs.creator_preferences.get(creator_lower.as_str()).copied().unwrap_or(0.5);
                // Force to 10% of current value, minimum 0.05 — aggressive suppression.
                let suppressed = (current * 0.1_f32).max(0.05_f32);
                prefs.creator_preferences.insert(creator_lower.into(), suppressed);
            }
            // Same fast suppression for tags. Previously tags only received the
            // generic weak TAG_DELTA_FACTOR nudge applied above (identical
            // treatment to any other interaction type), inconsistent with
            // NotInterested's own doc comment ("the strongest negative signal").
            // A user rejecting a specific tag combination should suppress that
            // tag as hard as rejecting a creator does, not fade out over many
            // repeated rejections.
            for tag in &event.nft_tags {
                let current = prefs.tag_preferences.get(tag.as_str()).copied().unwrap_or(0.5);
                let suppressed = (current * 0.1_f32).max(0.05_f32);
                prefs.tag_preferences.insert(tag.as_str().into(), suppressed);
            }
        }
        _ => {}
    }
}

/// Evict one entry from `map` when it exceeds `max_size`, using `policy`.
/// Pure — no allocations beyond the map mutation itself.
pub fn evict(map: &mut HashMap<Box<str>, f32>, max_size: usize, policy: EvictionPolicy) {
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
            prefs.tag_preferences.insert((*tag).into(), *seed);
        }
    }
}

// ── Tests (pure function coverage) ───────────────────────────────────────────

#[cfg(test)]
mod tests;
