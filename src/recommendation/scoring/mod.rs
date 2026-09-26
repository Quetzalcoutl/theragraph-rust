//! Scoring sub-module
//!
//! Pure scoring logic extracted from `engine.rs`:
//! - Domain types: `ScoringWeights`, `ScoringContext`, `ScoringSession`,
//!   `ScoringStrategy`, `WeightedScoring`, `FollowingScoring`
//! - Free scoring functions: `compute_type_affinity_score`,
//!   `compute_creator_affinity_score`, `compute_feature_scores`,
//!   `calculate_score_static`, `compute_recency_score`,
//!   `apply_diversity_shuffle_static`, `score_batch`
//!
//! None of the items here touch the database, Redis, or async I/O — they are
//! purely CPU-bound transforms of `CandidateNft` + `ScoringFeatures` +
//! `UserPreferences` → `ScoredNft`.  This makes them straightforward to unit
//! test in isolation.

mod compute;
pub use compute::*;

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::recommendation::{
    candidate_repository::CandidateNft,
    features::ScoringFeatures,
    preferences::UserPreferences,
    types::ContentType,
};

// ── Output types ──────────────────────────────────────────────────────────────

/// A scored recommendation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredNft {
    pub nft_id: Box<str>,
    pub token_id: i64,
    pub contract_address: Box<str>,
    pub score: f32,
    pub reason: RecommendationReason,
    pub contract_type: Box<str>,
    pub creator_address: Box<str>,
    #[serde(skip_serializing_if = "<[_]>::is_empty", default)]
    pub tags: Box<[Arc<str>]>,
}

/// Why this NFT was recommended
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationReason {
    /// Matches user's tag preferences
    TagMatch { matching_tags: Box<[Arc<str>]> },
    /// From a creator user has engaged with
    CreatorAffinity { creator: Box<str> },
    /// Similar content type preference
    ContentTypeMatch { content_type: ContentType },
    /// Currently trending
    Trending { trending_score: f32 },
    /// From someone user follows
    Following { followee: Box<str> },
    /// High quality/engagement
    HighEngagement { engagement_score: f32 },
    /// Serendipity - introducing variety
    Discovery,
}

// ── Weight type ───────────────────────────────────────────────────────────────

/// Recommendation weights (can be tuned)
#[derive(Debug, Clone, Copy)]
pub struct ScoringWeights {
    pub tag_match: f32,
    pub creator_affinity: f32,
    pub content_type: f32,
    pub trending: f32,
    pub engagement: f32,
    pub quality: f32,
    pub recency: f32,
    pub diversity_penalty: f32,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        // ByteGraph-inspired weights: prioritize personalization heavily
        Self {
            tag_match: 0.30,         // 30% weight on tag matching; raised from 0.03 original but
                                     // reduced from 0.35 so positive signals sum to exactly 1.00:
                                     // tag(0.30)+creator(0.20)+type(0.25)+trending(0.05)+
                                     // engagement(0.05)+quality(0.05)+recency(0.10) = 1.00.
                                     // The 1.6× multi-match multiplier in compute_feature_scores
                                     // can push tag contribution above 0.30 in rare cases; that
                                     // headroom rewards content with 3+ strong tag overlaps while
                                     // the final clamp(0,1) keeps scores bounded.
            creator_affinity: 0.20,  // 20% weight on creator preference (increased)
            content_type: 0.25,      // 25% weight on content type match (increased)
            trending: 0.05,          // 5% weight on trending score (reduced)
            engagement: 0.05,        // 5% weight on overall engagement (reduced)
            quality: 0.05,           // 5% weight on quality score (reduced)
            recency: 0.10,           // 10% weight on recency — raised from 0.03; NFTs mint
                                     // on a blockchain ledger so "freshness" matters more
                                     // than the old 3% weight acknowledged.
            diversity_penalty: 0.15, // 15% max penalty for creator/tag saturation — raised
                                     // from 0.02 which was so small it had no practical effect;
                                     // see compute_feature_scores for how this scales.
        }
    }
}

// ── Scoring context ───────────────────────────────────────────────────────────

/// Context used for scoring a single NFT
pub struct ScoringContext<'a> {
    pub prefs: &'a UserPreferences,
    pub contract_type: &'a str,
    pub creator_address: &'a str,
    pub created_at: &'a str,
    /// Unix timestamp (seconds) captured once before the Rayon batch begins.
    /// Eliminates one Utc::now() vDSO syscall per NFT in the hot path.
    pub now_unix: i64,
    pub features: &'a Option<ScoringFeatures>,
    pub seen_creators: &'a HashMap<Box<str>, usize>,
    pub seen_tags: &'a HashMap<Arc<str>, usize>,
}

// ── Strategy seam ─────────────────────────────────────────────────────────────

/// Pluggable scoring algorithm.
///
/// `WeightedScoring` is the production implementation; a second adapter (e.g.
/// an ML-based scorer or a test double) makes this a real seam, not hypothetical.
pub trait ScoringStrategy: Send + Sync {
    fn score(&self, ctx: &ScoringContext<'_>) -> (f32, RecommendationReason);
}

/// Production strategy: weighted linear combination of signals.
pub struct WeightedScoring {
    pub weights: ScoringWeights,
}

impl WeightedScoring {
    #[allow(dead_code)]
    pub fn new(weights: ScoringWeights) -> Self {
        Self { weights }
    }
}

impl Default for WeightedScoring {
    fn default() -> Self {
        Self { weights: ScoringWeights::default() }
    }
}

impl ScoringStrategy for WeightedScoring {
    fn score(&self, ctx: &ScoringContext<'_>) -> (f32, RecommendationReason) {
        calculate_score_static(ctx, &self.weights)
    }
}

/// Following-feed strategy: chronological-first scoring.
///
/// Weights recency heavily (0.7) and uses engagement as a secondary signal (0.3).
/// The `Following` reason always names the creator so the client can surface
/// "posted by @creator" labels.
pub struct FollowingScoring;

impl ScoringStrategy for FollowingScoring {
    fn score(&self, ctx: &ScoringContext<'_>) -> (f32, RecommendationReason) {
        let recency = compute_recency_score(ctx.created_at, ctx.now_unix);
        let engagement = ctx.features.as_ref().map(|f| f.engagement_score).unwrap_or(0.0);
        // TG-01: clamp final score to [0.0, 1.0] so a future-timestamped NFT
        // (age_hours < 0 → exp() > 1.0 → recency > 1.0) cannot produce a score
        // that overflows serialization or biases ranking beyond the intended range.
        let score = (recency * 0.7 + engagement * 0.3).clamp(0.0, 1.0);
        debug_assert!((0.0f32..=1.0f32).contains(&score), "FollowingScoring score {score} out of bounds");
        let reason = RecommendationReason::Following {
            followee: ctx.creator_address.into(),
        };
        (score, reason)
    }
}

// ── Scoring session ───────────────────────────────────────────────────────────

/// A one-shot scoring pass that captures a consistent weight snapshot and owns
/// all rayon parallelism. Construct with `RecommendationEngine::begin_session`
/// (uses the default `WeightedScoring` strategy) or
/// `RecommendationEngine::begin_session_with_strategy` (custom strategy).
///
/// # Why a separate type?
/// Both `get_enhanced_feed` and `get_recommendations` contained an identical
/// `spawn_blocking` block: snapshot weights, build strategy, par_iter chunks,
/// flat_map score_batch, cross-chunk creator dedup, sort. Extracting the block
/// into `ScoringSession::score` means any future change to the scoring loop
/// happens in exactly one place.
///
/// # Weight consistency
/// For `WeightedScoring`, the weight snapshot is taken at construction time.
/// Any `update_weights` call that fires during `score()` does not affect this
/// pass — preventing inconsistent scores within a single feed response.
pub struct ScoringSession {
    strategy: Box<dyn ScoringStrategy>,
    prefs: UserPreferences,
    /// Pre-computed tag → decayed boost map from the current session's interactions.
    /// Empty when no session signals are available (graceful no-op).
    session_tag_boosts: HashMap<Box<str>, f32>,
    /// Pre-computed creator → decayed boost map.
    session_creator_boosts: HashMap<Box<str>, f32>,
}

impl ScoringSession {
    /// Construct a scoring session from pre-loaded boost maps.
    ///
    /// Prefer calling `RecommendationEngine::begin_session` or
    /// `RecommendationEngine::begin_session_with_strategy`, which handle weight
    /// snapshotting — this constructor is the single assembly point for the
    /// `ScoringSession` value. External struct-update patterns (`..session`) are
    /// no longer needed: callers pass boost maps directly at construction time.
    pub(crate) fn new(
        strategy: Box<dyn ScoringStrategy>,
        prefs: UserPreferences,
        session_tag_boosts: HashMap<Box<str>, f32>,
        session_creator_boosts: HashMap<Box<str>, f32>,
    ) -> Self {
        Self {
            strategy,
            prefs,
            session_tag_boosts,
            session_creator_boosts,
        }
    }

    /// Score `candidates` in parallel, dedup same-creator, and sort by score descending.
    ///
    /// Runs inside `tokio::task::spawn_blocking` so the async executor stays free
    /// during the CPU-bound rayon work.
    pub fn score(self, candidates: Vec<(CandidateNft, Option<ScoringFeatures>)>) -> Vec<ScoredNft> {
        use rayon::prelude::*;

        let strategy = self.strategy;
        let prefs = self.prefs;
        let chunk_size = (candidates.len() / rayon::current_num_threads().max(1)).max(50);

        // Pre-compute now_unix once so compute_recency_score avoids a vDSO syscall
        // for each of the 500 candidates in the Rayon hot path.
        let now_unix: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Per-chunk diversity maps — parallel chunks don't share mutable state.
        // Cross-chunk creator dedup happens after the merge below.
        let strategy_ref: &dyn ScoringStrategy = &*strategy;
        let mut scored: Vec<ScoredNft> = candidates
            .into_par_iter()
            .chunks(chunk_size)
            .flat_map(|chunk| {
                let n = chunk.len();
                let mut seen_c = HashMap::with_capacity(n);
                let mut seen_t = HashMap::with_capacity(n * 4);
                score_batch(chunk, &prefs, strategy_ref, &mut seen_c, &mut seen_t, now_unix)
            })
            .collect();

        // Cross-chunk creator dedup: the same creator can appear at the top of
        // multiple chunks. Keep only the highest-scoring item per creator before
        // the final sort.
        let mut seen_creators: std::collections::HashSet<Box<str>> =
            std::collections::HashSet::with_capacity(scored.len() / 2 + 1);
        // RS-05: sort before dedup so retain keeps the highest-scored item per creator.
        // retain preserves relative order, so scored is still sorted descending afterwards.
        // The previous second nan_safe_sort_desc call was a no-op O(n log n) waste.
        nan_safe_sort_desc(&mut scored);
        scored.retain(|item| seen_creators.insert(item.creator_address.clone()));

        // YouTube-style session recency boost: add up to +0.125 on top of the
        // long-term score for tags/creators the user interacted with this session.
        // (boost raw value capped at 0.5, multiplied by 0.25 → max contribution 0.125)
        // Applied after dedup so we don't boost already-suppressed duplicates.
        if !self.session_tag_boosts.is_empty() || !self.session_creator_boosts.is_empty() {
            for item in &mut scored {
                let tag_boost: f32 = item
                    .tags
                    .iter()
                    .map(|t| {
                        self.session_tag_boosts
                            .get(t.as_ref())
                            .copied()
                            .unwrap_or(0.0)
                    })
                    .sum::<f32>()
                    .min(0.5);
                let creator_boost = self
                    .session_creator_boosts
                    .get(item.creator_address.as_ref())
                    .copied()
                    .unwrap_or(0.0)
                    .min(0.5);
                let boost = (tag_boost + creator_boost * 0.5).min(0.5);
                item.score = (item.score + boost * 0.25).clamp(0.0, 1.0);
            }
            nan_safe_sort_desc(&mut scored);
        }

        // Callers that mutate scores after this call (e.g. FoF boost in
        // get_recommendations) are responsible for re-sorting afterwards.

        scored
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;

#[cfg(test)]
mod session_boost_tests;
