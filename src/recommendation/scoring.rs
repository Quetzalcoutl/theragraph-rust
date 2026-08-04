//! Scoring sub-module
//!
//! Pure scoring logic extracted from `engine.rs`:
//! - Domain types: `ScoringWeights`, `ScoringContext`, `ScoringSession`,
//!   `ScoringStrategy`, `WeightedScoring`, `FollowingScoring`
//! - Free scoring functions: `compute_type_affinity_score`,
//!   `compute_creator_affinity_score`, `compute_feature_scores`,
//!   `calculate_score_static`, `compute_recency_score`,
//!   `apply_diversity_shuffle_static`, `score_batch`, `score_single_nft`
//!
//! None of the items here touch the database, Redis, or async I/O — they are
//! purely CPU-bound transforms of `CandidateNft` + `ScoringFeatures` +
//! `UserPreferences` → `ScoredNft`.  This makes them straightforward to unit
//! test in isolation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::recommendation::{
    cache::SessionSignal,
    candidate_repository::CandidateNft,
    features::ScoringFeatures,
    preferences::UserPreferences,
    types::ContentType,
};

// ── Output types ──────────────────────────────────────────────────────────────

/// A scored recommendation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredNft {
    pub nft_id: String,
    pub token_id: i64,
    pub contract_address: String,
    pub score: f32,
    pub reason: RecommendationReason,
    pub contract_type: String,
    pub creator_address: String,
    pub tags: Vec<String>,
}

/// Why this NFT was recommended
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationReason {
    /// Matches user's tag preferences
    TagMatch { matching_tags: Vec<String> },
    /// From a creator user has engaged with
    CreatorAffinity { creator: String },
    /// Similar content type preference
    ContentTypeMatch { content_type: String },
    /// Currently trending
    Trending { trending_score: f32 },
    /// From someone user follows
    Following { followee: String },
    /// High quality/engagement
    HighEngagement { engagement_score: f32 },
    /// Serendipity - introducing variety
    Discovery,
}

// ── Weight type ───────────────────────────────────────────────────────────────

/// Recommendation weights (can be tuned)
#[derive(Debug, Clone)]
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
    pub features: &'a Option<ScoringFeatures>,
    pub seen_creators: &'a HashMap<String, usize>,
    pub seen_tags: &'a HashMap<String, usize>,
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
        let recency = compute_recency_score(ctx.created_at);
        let engagement = ctx.features.as_ref().map(|f| f.engagement_score).unwrap_or(0.0);
        // TG-01: clamp final score to [0.0, 1.0] so a future-timestamped NFT
        // (age_hours < 0 → exp() > 1.0 → recency > 1.0) cannot produce a score
        // that overflows serialization or biases ranking beyond the intended range.
        let score = (recency * 0.7 + engagement * 0.3).clamp(0.0, 1.0);
        debug_assert!((0.0f32..=1.0f32).contains(&score), "FollowingScoring score {score} out of bounds");
        let reason = RecommendationReason::Following {
            followee: ctx.creator_address.to_string(),
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
    session_tag_boosts: HashMap<String, f32>,
    /// Pre-computed creator → decayed boost map.
    session_creator_boosts: HashMap<String, f32>,
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
        session_tag_boosts: HashMap<String, f32>,
        session_creator_boosts: HashMap<String, f32>,
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

        // Per-chunk diversity maps — parallel chunks don't share mutable state.
        // Cross-chunk creator dedup happens after the merge below.
        let strategy_ref: &dyn ScoringStrategy = &*strategy;
        let mut scored: Vec<ScoredNft> = candidates
            .into_par_iter()
            .chunks(chunk_size)
            .flat_map(|chunk| {
                let mut seen_c = HashMap::new();
                let mut seen_t = HashMap::new();
                score_batch(chunk, &prefs, strategy_ref, &mut seen_c, &mut seen_t)
            })
            .collect();

        // Cross-chunk creator dedup: the same creator can appear at the top of
        // multiple chunks. Keep only the highest-scoring item per creator before
        // the final sort.
        let mut seen_creators: std::collections::HashSet<String> =
            std::collections::HashSet::new();
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
                            .get(&t.to_lowercase())
                            .copied()
                            .unwrap_or(0.0)
                    })
                    .sum::<f32>()
                    .min(0.5);
                let creator_boost = self
                    .session_creator_boosts
                    .get(&item.creator_address.to_lowercase())
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

// ── Free scoring helpers ──────────────────────────────────────────────────────

/// Sort `items` by score descending, NaN scores sort last.
pub(crate) fn nan_safe_sort_desc(items: &mut Vec<ScoredNft>) {
    items.sort_unstable_by(|a, b| match (a.score.is_nan(), b.score.is_nan()) {
        (true, _) => std::cmp::Ordering::Greater,
        (_, true) => std::cmp::Ordering::Less,
        _ => b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal),
    });
}

/// Score a single NFT without a full `RecommendationEngine` instance.
///
/// Returns `None` when the NFT has no ID (skip sentinel).
/// Delegates to `score_batch` so the two never diverge.
#[allow(dead_code)]
pub fn score_single_nft(
    nft: &CandidateNft,
    features: &Option<ScoringFeatures>,
    prefs: &UserPreferences,
    strategy: &impl ScoringStrategy,
    seen_creators: &HashMap<String, usize>,
    seen_tags: &HashMap<String, usize>,
) -> Option<ScoredNft> {
    let mut sc = seen_creators.clone();
    let mut st = seen_tags.clone();
    score_batch(
        vec![(nft.clone(), features.clone())],
        prefs,
        strategy,
        &mut sc,
        &mut st,
    )
    .into_iter()
    .next()
}

/// ByteGraph-inspired content type affinity scoring with dynamic boosting.
/// Uses the user's actual affinity values directly (already normalized 0-1).
pub(crate) fn compute_type_affinity_score(
    weights: &ScoringWeights,
    contract_type: &str,
    prefs: &UserPreferences,
) -> (f32, Option<RecommendationReason>) {
    let type_affinity = ContentType::from_str(contract_type)
        .map(|ct| prefs.affinity_for(&ct))
        .unwrap_or(0.5);

    // Find user's strongest affinity for adaptive boosting
    let max_affinity = prefs
        .snap_affinity
        .max(prefs.art_affinity)
        .max(prefs.music_affinity)
        .max(prefs.flix_affinity);

    // Apply non-linear boost for high affinity (ByteGraph-style)
    // Extra boost if this matches user's primary interest
    let is_primary = type_affinity >= max_affinity * 0.95;
    // Clamp boosted_affinity to 1.0 before multiplying by the weight.
    // Without the clamp: at type_affinity=1.0, base_boost = 0.5 + 0.5^0.7 ≈ 1.116,
    // and with is_primary=true that becomes ≈1.228. Multiplied by weights.content_type
    // (0.25) the contribution reaches ≈0.307 instead of the intended max of 0.25,
    // silently inflating total above 1.0 and defeating the scoring normalization.
    let boosted_affinity = if type_affinity > 0.5 {
        let base_boost = 0.5 + (type_affinity - 0.5).powf(0.7);
        let adjusted = if is_primary { base_boost * 1.1 } else { base_boost };
        adjusted.min(1.0)
    } else {
        type_affinity * 0.8 // Reduce low affinity more
    };

    let type_score = boosted_affinity * weights.content_type;
    let reason = if type_affinity > 0.55 {
        Some(RecommendationReason::ContentTypeMatch {
            content_type: contract_type.to_string(),
        })
    } else {
        None
    };
    (type_score, reason)
}

/// ByteGraph-inspired creator affinity scoring.
///
/// EFF-002: no .to_lowercase() — creator_address is pre-normalized at write
/// time by VID-CASE-001, so the allocation is wasted work.
pub(crate) fn compute_creator_affinity_score(
    weights: &ScoringWeights,
    creator: &str,
    prefs: &UserPreferences,
) -> (f32, Option<RecommendationReason>) {
    let creator_affinity = prefs
        .creator_preferences
        .get(creator)
        .copied()
        .unwrap_or(0.3);

    // Strong boost for known creators the user has engaged with
    let boosted = if creator_affinity > 0.5 {
        creator_affinity * 1.5 // 50% boost for liked creators
    } else {
        creator_affinity * 0.5 // Reduce for unknown creators
    };

    let creator_score = boosted.min(1.0) * weights.creator_affinity;
    let reason = if creator_affinity > 0.5 {
        Some(RecommendationReason::CreatorAffinity {
            creator: creator.to_string(),
        })
    } else {
        None
    };
    (creator_score, reason)
}

/// ByteGraph-inspired feature scoring with collaborative signals.
pub(crate) fn compute_feature_scores(
    weights: &ScoringWeights,
    f: &ScoringFeatures,
    prefs: &UserPreferences,
    creator_address: &str,
    seen_creators: &HashMap<String, usize>,
    seen_tags: &HashMap<String, usize>,
) -> (f32, Option<RecommendationReason>) {
    let mut total = 0.0f32;
    // Tracks (best_score, best_reason) together — no separate max_score proxy.
    let mut primary: Option<(f32, RecommendationReason)> = None;

    // ByteGraph-style tag matching with exponential boost for multiple matches
    // Tags include user-provided hashtags (max 3) from metadata for personalized recommendations
    // REC-P3-06: pre-allocate to the NFT's tag count — typical range 1-10, no realloc needed.
    let mut matching_tags = Vec::with_capacity(f.tags.len());
    let mut tag_score_sum = 0.0f32;
    let mut match_count = 0;

    // Dynamic threshold based on user's tag diversity
    let tag_threshold = if prefs.tag_preferences.len() > 20 { 0.65 } else { 0.6 };

    for tag in f.tags.iter() {
        // EFF-002: tags in ScoringFeatures are stored lowercase (enforced in extract_features),
        // so no .to_lowercase() allocation is needed for the fallback lookup.
        let pref = prefs
            .tag_preferences
            .get(tag.as_str())
            .copied()
            .unwrap_or(0.3); // Lower default for unmatched tags

        // Tags are sorted alphabetically in ScoringFeatures; a position-based idx<3
        // boost would reward alphabetically-early tags, not user-supplied hashtags.
        if pref > tag_threshold {
            matching_tags.push(tag.clone());
            tag_score_sum += pref;
            match_count += 1;
        }
    }

    // Exponential boost for multiple tag matches (ByteGraph collaborative signal)
    let tag_match_score = if !matching_tags.is_empty() {
        let base_score = tag_score_sum / matching_tags.len() as f32;
        // Apply exponential boost: 1 match = 1x, 2 matches = 1.3x, 3+ matches = 1.6x
        let match_multiplier = 1.0 + (match_count as f32 - 1.0) * 0.15;
        base_score * weights.tag_match * match_multiplier.min(1.6)
    } else {
        // Penalty for NFTs with no tag overlap
        -0.1 * weights.tag_match
    };
    total += tag_match_score;

    if tag_match_score > 0.0 && !matching_tags.is_empty() {
        if primary.as_ref().map_or(true, |(s, _)| tag_match_score > *s) {
            primary = Some((
                tag_match_score,
                RecommendationReason::TagMatch { matching_tags },
            ));
        }
    }

    // Trending (reduced weight in ByteGraph-style - personalization trumps trending)
    let trending_contrib = f.trending_score * weights.trending;
    total += trending_contrib;
    if f.trending_score > 0.7 {
        if primary.as_ref().map_or(true, |(s, _)| trending_contrib > *s) {
            primary = Some((
                trending_contrib,
                RecommendationReason::Trending { trending_score: f.trending_score },
            ));
        }
    }

    // Engagement
    let engagement_contrib = f.engagement_score * weights.engagement;
    total += engagement_contrib;
    if f.engagement_score > 0.8 {
        if primary.as_ref().map_or(true, |(s, _)| engagement_contrib > *s) {
            primary = Some((
                engagement_contrib,
                RecommendationReason::HighEngagement { engagement_score: f.engagement_score },
            ));
        }
    }

    // Quality
    total += f.quality_score * weights.quality;

    // ByteGraph diversity penalties with diminishing returns
    let creator_count = seen_creators.get(creator_address).copied().unwrap_or(0);
    if creator_count > 2 {
        // Logarithmic penalty: more same-creator content = exponentially less appealing.
        // Cap at weights.diversity_penalty so a single creator can never score below
        // base-0 regardless of how many items they have in the candidate set.
        let penalty_multiplier = (creator_count as f32).ln() / 2.0;
        total -= (weights.diversity_penalty * penalty_multiplier).min(weights.diversity_penalty);
    }

    // Tag oversaturation with smart thresholding
    let tag_oversaturation: f32 = f
        .tags
        .iter()
        .map(|t| seen_tags.get(t).copied().unwrap_or(0) as f32)
        .sum::<f32>()
        / f.tags.len().max(1) as f32;
    if tag_oversaturation > 4.0 {
        // Square root penalty for smoother degradation
        let penalty = (tag_oversaturation - 4.0).sqrt() * 0.03;
        total -= weights.diversity_penalty * penalty;
    }

    (total, primary.map(|(_, reason)| reason))
}

/// Static scoring entry point (Niko Matsakis optimization).
/// Allows Rayon to process scores without a `self` reference.
pub(crate) fn calculate_score_static(
    ctx: &ScoringContext<'_>,
    weights: &ScoringWeights,
) -> (f32, RecommendationReason) {
    // Collect (score, optional reason) from each sub-scorer.
    // Adding a new signal means appending one entry here — no inline
    // max-tracking block needed.
    let feature_pair = ctx.features.as_ref().map(|f| {
        compute_feature_scores(weights, f, ctx.prefs, ctx.creator_address, ctx.seen_creators, ctx.seen_tags)
    });

    let signal_pairs: &[(f32, Option<RecommendationReason>)] = &[
        compute_type_affinity_score(weights, ctx.contract_type, ctx.prefs),
        compute_creator_affinity_score(weights, ctx.creator_address, ctx.prefs),
        feature_pair.unwrap_or((0.0, None)),
    ];

    let mut total = 0.0f32;
    let mut primary_reason = RecommendationReason::Discovery;
    let mut max_reason_score = 0.0f32;

    for (sig_score, sig_reason) in signal_pairs {
        total += sig_score;
        if let Some(r) = sig_reason {
            if *sig_score > max_reason_score {
                max_reason_score = *sig_score;
                primary_reason = r.clone();
            }
        }
    }

    // Recency bonus (no associated reason — it is never the primary signal)
    total += compute_recency_score(ctx.created_at) * weights.recency;

    // Clamp to 0-1; NaN (e.g. from 0.0/0.0 in feature paths) -> 0.0
    let score = if total.is_nan() {
        tracing::warn!(
            creator = ctx.creator_address,
            contract_type = ctx.contract_type,
            "NaN score detected — defaulting to 0.0"
        );
        0.0f32
    } else {
        total.clamp(0.0, 1.0)
    };

    // Hugh Blair-Smith / Raph Levien: lock down the invariant at the source.
    // ScoredNft.score MUST be in [0.0, 1.0] per FeedSource contract.
    // nan_safe_sort_desc existence proves NaN has reached production; assert here
    // in dev/test so the source is caught rather than silently propagating.
    debug_assert!(!score.is_nan(), "score is NaN after clamping for creator={}", ctx.creator_address);
    debug_assert!((0.0f32..=1.0f32).contains(&score), "score {score} out of [0,1] bounds");

    (score, primary_reason)
}

/// Parse a timestamp and return an exponential recency score.
/// Newer = higher score; half-life of 24 hours.
pub(crate) fn compute_recency_score(created_at: &str) -> f32 {
    match chrono::DateTime::parse_from_rfc3339(created_at) {
        Ok(dt) => {
            // TG-01: clamp age to ≥ 0 so a future-dated timestamp (clock skew or
            // data error) does not produce a negative exponent argument that makes
            // exp() return a value > 1.0 (= f32::INFINITY for very far-future dates),
            // which then breaks JSON serialization and biases ranking.
            let age_hours = (chrono::Utc::now() - dt.with_timezone(&chrono::Utc))
                .num_hours()
                .max(0) as f32;
            // Exponential decay with 168-hour (7-day) half-life.
            // Previous 24h half-life caused week-old NFTs to score near-zero for recency
            // (e^(-168/24) = e^(-7) ≈ 0.001) — effectively killing any NFT older than 3 days
            // even when it had strong tag/engagement signals. 168h keeps content competitive
            // for a natural discovery window while still rewarding genuinely new mints.
            (-age_hours / 168.0).exp()
        }
        Err(_) => 0.5, // Default if parse fails
    }
}

/// Apply slight randomization to top results for discovery, then enforce
/// a 60% content-type cap so no single content_type dominates the final feed.
///
/// Static version for use in parallel contexts (Andrew Gallant optimization).
///
/// The cap matters because a user with high art affinity could end up with a
/// feed that is 90% art and 0% music, which destroys recommendation breadth.
/// 60% is generous enough that a genuine affinity preference still shows through
/// while still guaranteeing at least one other content type per 5 results.
pub(crate) fn apply_diversity_shuffle_static(mut scored: Vec<ScoredNft>, limit: usize) -> Vec<ScoredNft> {
    use rand::seq::SliceRandom;
    use std::collections::HashMap;

    if scored.len() <= limit {
        return scored;
    }

    // Take top 80% deterministically, shuffle remaining 20% slots
    let deterministic_count = (limit as f32 * 0.8) as usize;
    let shuffle_count = limit - deterministic_count;

    let mut result: Vec<ScoredNft> = scored.drain(..deterministic_count).collect();

    // From remaining, pick some randomly for discovery
    let mut rng = rand::thread_rng();
    let remaining: Vec<_> = scored.into_iter().take(shuffle_count * 3).collect();

    if !remaining.is_empty() {
        let chosen: Vec<_> = remaining
            .choose_multiple(&mut rng, shuffle_count.min(remaining.len()))
            .cloned()
            .collect();
        result.extend(chosen);
    }

    // Content-type cap: no single type may exceed 60% of the final `limit` slots.
    // Applied as a post-filter so the highest-scored items of each type survive;
    // overflow items are dropped rather than reordered.
    let max_per_type = ((limit as f32 * 0.60).ceil() as usize).max(1);
    let mut type_counts: HashMap<String, usize> = HashMap::new();
    result.retain(|item| {
        let count = type_counts.entry(item.contract_type.clone()).or_insert(0);
        if *count < max_per_type {
            *count += 1;
            true
        } else {
            false
        }
    });

    result
}

/// Pre-compute tag and creator boost maps from a user's session signals.
///
/// Each signal contributes `weight × exp(-age_secs / 1800)` (30-min half-life)
/// to every tag and creator it touched. Maps are clamped to [0, 1] before return.
/// Pass `now_unix` as `SystemTime::now().duration_since(UNIX_EPOCH).as_secs() as i64`
/// to avoid repeated system calls during batch scoring.
pub fn compute_session_boost_maps(
    signals: &[SessionSignal],
    now_unix: i64,
) -> (HashMap<String, f32>, HashMap<String, f32>) {
    let mut tag_boosts: HashMap<String, f32> = HashMap::new();
    let mut creator_boosts: HashMap<String, f32> = HashMap::new();

    for sig in signals {
        let age_secs = (now_unix - sig.ts_unix).max(0) as f32;
        // 30-min half-life: exp(-age / 1800). At 0 min → 1.0, at 30 min → 0.5, at 2h → 0.09.
        let decay = (-age_secs / 1800.0_f32).exp();
        let decayed = sig.interaction_weight * decay;

        for tag in &sig.tags {
            *tag_boosts.entry(tag.to_lowercase()).or_insert(0.0) += decayed;
        }
        if let Some(ref creator) = sig.creator {
            *creator_boosts
                .entry(creator.to_lowercase())
                .or_insert(0.0) += decayed;
        }
    }

    for v in tag_boosts.values_mut() {
        *v = v.clamp(0.0, 1.0);
    }
    for v in creator_boosts.values_mut() {
        *v = v.clamp(0.0, 1.0);
    }

    (tag_boosts, creator_boosts)
}

/// Score a candidate batch into `ScoredNft` values.
///
/// Pure function — no DB, no async, no side effects. Callers own the
/// seen-creator/tag maps; pass empty maps when diversity tracking is not
/// needed (e.g. parallel chunks that later merge).
pub(crate) fn score_batch(
    candidates: Vec<(CandidateNft, Option<ScoringFeatures>)>,
    prefs: &UserPreferences,
    strategy: &dyn ScoringStrategy,
    seen_creators: &mut HashMap<String, usize>,
    seen_tags: &mut HashMap<String, usize>,
) -> Vec<ScoredNft> {
    let mut scored = Vec::with_capacity(candidates.len());

    for (nft, features) in candidates {
        // EFF-003: destructure CandidateNft to enable moves instead of clones.
        // contract_address and creator_address are moved directly into ScoredNft;
        // id, contract_type, created_at are moved out of their Options via unwrap_or_default.
        let CandidateNft { id, token_id, contract_address, contract_type, creator_address, created_at } = nft;
        let nft_id = match id {
            Some(id) => id,
            None => continue,
        };
        let contract_type = contract_type.unwrap_or_default();
        let created_at = created_at.unwrap_or_default();

        let ctx = ScoringContext {
            prefs,
            contract_type: &contract_type,
            creator_address: &creator_address,
            created_at: &created_at,
            features: &features,
            seen_creators,
            seen_tags,
        };

        let (score, reason) = strategy.score(&ctx);

        // RS-06: avoid cloning the key string when the entry already exists —
        // most candidates share creators/tags so the majority of clones were discarded.
        // EFF-003: for new creators, one clone is unavoidable (HashMap needs owned key);
        // then creator_address itself is moved into ScoredNft — net saving vs before.
        if let Some(count) = seen_creators.get_mut(&creator_address) {
            *count += 1;
        } else {
            seen_creators.insert(creator_address.clone(), 1);
        }
        if let Some(tags) = features.as_ref().map(|f| &f.tags) {
            for tag in tags {
                if let Some(count) = seen_tags.get_mut(tag) {
                    *count += 1;
                } else {
                    seen_tags.insert(tag.clone(), 1);
                }
            }
        }

        scored.push(ScoredNft {
            nft_id,
            token_id,
            contract_address,   // moved — no clone
            score,
            reason,
            contract_type,
            creator_address,    // moved — no clone
            tags: features.map(|f| f.tags).unwrap_or_default(),
        });
    }

    scored
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_features(
        tags: Vec<&str>,
        engagement: f32,
        trending: f32,
        quality: f32,
    ) -> ScoringFeatures {
        ScoringFeatures {
            tags: tags.into_iter().map(|t| t.to_string()).collect(),
            engagement_score: engagement,
            trending_score: trending,
            quality_score: quality,
        }
    }

    fn make_candidate(id: &str, creator: &str, contract_type: &str) -> CandidateNft {
        CandidateNft {
            id: Some(id.to_string()),
            token_id: 1,
            contract_address: format!("0x{id}"),
            contract_type: Some(contract_type.to_string()),
            creator_address: creator.to_string(),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
        }
    }

    // ── compute_type_affinity_score ───────────────────────────────────────────

    #[test]
    fn type_affinity_high_pref_returns_positive_score_and_reason() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.art_affinity = 0.8;

        let (score, reason) = compute_type_affinity_score(&weights, "art", &prefs);
        assert!(score > 0.0, "expected positive score for high art affinity");
        match reason {
            Some(RecommendationReason::ContentTypeMatch { content_type }) => {
                assert_eq!(content_type, "art")
            }
            _ => panic!("expected ContentTypeMatch reason, got {reason:?}"),
        }
    }

    #[test]
    fn type_affinity_low_pref_returns_no_reason() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.snap_affinity = 0.2; // well below the 0.55 threshold

        let (_score, reason) = compute_type_affinity_score(&weights, "snap", &prefs);
        assert!(
            reason.is_none(),
            "low affinity should not produce a ContentTypeMatch reason"
        );
    }

    #[test]
    fn type_affinity_primary_content_gets_extra_boost() {
        // art is the user's primary content type (highest affinity)
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.art_affinity = 0.9;
        prefs.snap_affinity = 0.2;
        prefs.music_affinity = 0.2;
        prefs.flix_affinity = 0.2;

        let (art_score, _) = compute_type_affinity_score(&weights, "art", &prefs);
        // Non-primary type at the same raw affinity level for comparison
        let mut prefs2 = prefs.clone();
        prefs2.music_affinity = 0.9; // tie — both are now primary
        let (music_score, _) = compute_type_affinity_score(&weights, "music", &prefs2);

        // Both are primary when tied, so they should be equal (within float epsilon)
        assert!(
            (art_score - music_score).abs() < 1e-5,
            "tied primaries should score equally: art={art_score}, music={music_score}"
        );
    }

    #[test]
    fn type_affinity_unknown_contract_type_uses_fallback() {
        // ContentType::from_str returns None for unknown types → fallback affinity 0.5
        let weights = ScoringWeights::default();
        let prefs = UserPreferences::default();

        // "video" is not a known ContentType variant
        let (score, _) = compute_type_affinity_score(&weights, "video", &prefs);
        // 0.5 affinity → boosted_affinity = 0.5 * 0.8 = 0.4 → score = 0.4 * weights.content_type
        // Just assert it is non-negative and below the max possible
        assert!(score >= 0.0);
        assert!(score <= weights.content_type);
    }

    #[test]
    fn type_affinity_all_four_content_types_dispatch() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.snap_affinity = 0.9;
        prefs.art_affinity = 0.7;
        prefs.music_affinity = 0.5;
        prefs.flix_affinity = 0.3;

        let (snap_score, _) = compute_type_affinity_score(&weights, "snap", &prefs);
        let (art_score, _) = compute_type_affinity_score(&weights, "art", &prefs);
        let (music_score, _) = compute_type_affinity_score(&weights, "music", &prefs);
        let (flix_score, _) = compute_type_affinity_score(&weights, "flix", &prefs);

        // Higher affinity → higher score (monotonicity over the four types)
        assert!(
            snap_score > art_score,
            "snap(0.9) should outscore art(0.7): {snap_score} vs {art_score}"
        );
        assert!(
            art_score > music_score,
            "art(0.7) should outscore music(0.5): {art_score} vs {music_score}"
        );
        // music is at 0.5, flix at 0.3; the low-end uses affinity * 0.8 so the
        // ordering still holds
        assert!(
            music_score > flix_score,
            "music(0.5) should outscore flix(0.3): {music_score} vs {flix_score}"
        );
    }

    // ── compute_creator_affinity_score ────────────────────────────────────────

    #[test]
    fn creator_affinity_known_creator_scores_higher_than_unknown() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        let creator = "0xdeadbeef";
        prefs.creator_preferences.insert(creator.to_string(), 0.8);

        let (known_score, _) = compute_creator_affinity_score(&weights, creator, &prefs);
        let (unknown_score, _) = compute_creator_affinity_score(&weights, "0xstranger", &prefs);

        assert!(
            known_score > unknown_score,
            "known creator should score higher: {known_score} vs {unknown_score}"
        );
    }

    #[test]
    fn creator_affinity_high_pref_yields_reason() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        let creator = "0xartist";
        prefs.creator_preferences.insert(creator.to_string(), 0.9);

        let (_score, reason) = compute_creator_affinity_score(&weights, creator, &prefs);
        match reason {
            Some(RecommendationReason::CreatorAffinity { creator: c }) => {
                assert_eq!(c, creator)
            }
            _ => panic!("expected CreatorAffinity reason, got {reason:?}"),
        }
    }

    #[test]
    fn creator_affinity_lookup_finds_normalized_key() {
        // VID-CASE-001: creator_address is pre-normalized to lowercase at every write
        // path. compute_creator_affinity_score no longer lowercases the key (EFF-002).
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.creator_preferences.insert("0xartist".to_string(), 0.9);

        let (known_score, _) = compute_creator_affinity_score(&weights, "0xartist", &prefs);
        let (unknown_score, _) = compute_creator_affinity_score(&weights, "0xother", &prefs);

        assert!(
            known_score > unknown_score,
            "known lowercase creator should score higher than unknown: known={known_score} unknown={unknown_score}"
        );
    }

    #[test]
    fn creator_affinity_score_capped_at_weight() {
        // Even with affinity=1.0 * 1.5 boost the result is min(1.0) * weight
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.creator_preferences.insert("0xcreator".to_string(), 1.0);

        let (score, _) = compute_creator_affinity_score(&weights, "0xcreator", &prefs);
        assert!(
            score <= weights.creator_affinity,
            "score {score} must not exceed weight {}", weights.creator_affinity
        );
    }

    // ── compute_feature_scores ────────────────────────────────────────────────

    #[test]
    fn feature_scores_tag_match_single_tag() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.tag_preferences.insert("landscape".to_string(), 0.8);

        let f = make_features(vec!["landscape"], 0.0, 0.0, 0.0);
        let (score, reason) =
            compute_feature_scores(&weights, &f, &prefs, "0xcreator", &HashMap::new(), &HashMap::new());

        assert!(score > 0.0, "single matching tag should produce positive score");
        match reason {
            Some(RecommendationReason::TagMatch { matching_tags }) => {
                assert_eq!(matching_tags, vec!["landscape".to_string()]);
            }
            _ => panic!("expected TagMatch reason, got {reason:?}"),
        }
    }

    #[test]
    fn feature_scores_multiple_tag_matches_score_higher_than_single() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.tag_preferences.insert("landscape".to_string(), 0.8);
        prefs.tag_preferences.insert("abstract".to_string(), 0.8);
        prefs.tag_preferences.insert("blue".to_string(), 0.8);

        let single = make_features(vec!["landscape"], 0.0, 0.0, 0.0);
        let multi = make_features(vec!["landscape", "abstract", "blue"], 0.0, 0.0, 0.0);

        let (s1, _) =
            compute_feature_scores(&weights, &single, &prefs, "0xcreator", &HashMap::new(), &HashMap::new());
        let (s3, _) =
            compute_feature_scores(&weights, &multi, &prefs, "0xcreator", &HashMap::new(), &HashMap::new());

        assert!(
            s3 > s1,
            "3 tag matches ({s3}) should outscores 1 tag match ({s1}) due to exponential boost"
        );
    }

    #[test]
    fn feature_scores_no_tags_does_not_panic() {
        let weights = ScoringWeights::default();
        let prefs = UserPreferences::default();
        let f = make_features(vec![], 0.5, 0.5, 0.5);

        // Must not panic; score may be 0 or small positive from engagement/trending/quality
        let (score, _) =
            compute_feature_scores(&weights, &f, &prefs, "0xcreator", &HashMap::new(), &HashMap::new());
        assert!(score.is_finite(), "score should be finite even with no tags");
    }

    #[test]
    fn feature_scores_trending_reason_fires_above_threshold() {
        let weights = ScoringWeights::default();
        let prefs = UserPreferences::default();
        let f = make_features(vec![], 0.0, 0.9, 0.0); // trending > 0.7

        let (_score, reason) =
            compute_feature_scores(&weights, &f, &prefs, "0xcreator", &HashMap::new(), &HashMap::new());
        assert!(
            matches!(reason, Some(RecommendationReason::Trending { .. })),
            "trending > 0.7 with no competing signal should yield Trending reason, got {reason:?}"
        );
    }

    #[test]
    fn feature_scores_engagement_reason_fires_above_threshold() {
        let weights = ScoringWeights::default();
        let prefs = UserPreferences::default();
        let f = make_features(vec![], 0.9, 0.0, 0.0); // engagement > 0.8

        let (_score, reason) =
            compute_feature_scores(&weights, &f, &prefs, "0xcreator", &HashMap::new(), &HashMap::new());
        assert!(
            matches!(reason, Some(RecommendationReason::HighEngagement { .. })),
            "engagement > 0.8 with no competing signal should yield HighEngagement reason, got {reason:?}"
        );
    }

    #[test]
    fn feature_scores_diversity_penalty_applies_after_many_same_creator() {
        let weights = ScoringWeights::default();
        let prefs = UserPreferences::default();
        let f = make_features(vec![], 0.5, 0.5, 0.5);

        let mut seen_few: HashMap<String, usize> = HashMap::new();
        seen_few.insert("0xabc".to_string(), 2); // at the boundary, no penalty yet

        let mut seen_many: HashMap<String, usize> = HashMap::new();
        seen_many.insert("0xabc".to_string(), 5); // > 2, penalty kicks in

        let (score_few, _) = compute_feature_scores(&weights, &f, &prefs, "0xabc", &seen_few, &HashMap::new());
        let (score_many, _) =
            compute_feature_scores(&weights, &f, &prefs, "0xabc", &seen_many, &HashMap::new());

        assert!(
            score_few >= score_many,
            "more same-creator items should not increase score: few={score_few} many={score_many}"
        );
    }

    // ── calculate_score_static ────────────────────────────────────────────────

    #[test]
    fn calculate_score_static_output_clamped_to_0_1() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        // Max out every signal so the raw sum would exceed 1.0
        prefs.art_affinity = 1.0;
        prefs.tag_preferences.insert("abstract".to_string(), 1.0);
        prefs.creator_preferences.insert("0xcreator".to_string(), 1.0);

        let f = make_features(vec!["abstract"], 1.0, 1.0, 1.0);
        let ctx = ScoringContext {
            prefs: &prefs,
            contract_type: "art",
            creator_address: "0xcreator",
            created_at: &chrono::Utc::now().to_rfc3339(),
            features: &Some(f),
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };

        let (score, _) = calculate_score_static(&ctx, &weights);
        assert!(
            (0.0..=1.0).contains(&score),
            "score must be clamped to [0, 1], got {score}"
        );
    }

    #[test]
    fn calculate_score_static_zero_input_returns_near_zero() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        // All affinities at 0.0 (lower than default 0.5)
        prefs.snap_affinity = 0.0;
        prefs.art_affinity = 0.0;
        prefs.music_affinity = 0.0;
        prefs.flix_affinity = 0.0;

        // No features at all
        let ctx = ScoringContext {
            prefs: &prefs,
            contract_type: "snap",
            creator_address: "0xunknown",
            // Use an old timestamp so recency bonus is near-zero
            created_at: "2020-01-01T00:00:00Z",
            features: &None,
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };

        let (score, _) = calculate_score_static(&ctx, &weights);
        assert!(score < 0.2, "near-zero inputs should produce a low score, got {score}");
    }

    #[test]
    fn calculate_score_static_discovery_reason_when_no_signal_wins() {
        // No tag prefs, no creator prefs, no features → Discovery fallback
        let weights = ScoringWeights::default();
        let prefs = UserPreferences::default();
        let ctx = ScoringContext {
            prefs: &prefs,
            contract_type: "art",
            creator_address: "0xnobody",
            created_at: "2020-01-01T00:00:00Z",
            features: &None,
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };

        let (_, reason) = calculate_score_static(&ctx, &weights);
        assert!(
            matches!(reason, RecommendationReason::Discovery),
            "no strong signal → should default to Discovery, got {reason:?}"
        );
    }

    // ── WeightedScoring strategy ──────────────────────────────────────────────

    #[test]
    fn weighted_scoring_score_method_delegates_correctly() {
        let strategy = WeightedScoring::default();
        let mut prefs = UserPreferences::default();
        prefs.art_affinity = 0.9;

        let ctx = ScoringContext {
            prefs: &prefs,
            contract_type: "art",
            creator_address: "0xartist",
            created_at: &chrono::Utc::now().to_rfc3339(),
            features: &None,
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };

        let (score, _) = strategy.score(&ctx);
        assert!(score > 0.0, "WeightedScoring should forward to calculate_score_static");
    }

    #[test]
    fn weighted_scoring_custom_weights_affect_output() {
        // Swap weights so content_type dominates
        let mut custom_weights = ScoringWeights::default();
        custom_weights.content_type = 0.9;
        custom_weights.tag_match = 0.0;
        custom_weights.creator_affinity = 0.0;

        let strategy = WeightedScoring::new(custom_weights);
        let mut prefs = UserPreferences::default();
        prefs.art_affinity = 0.9;

        let ctx = ScoringContext {
            prefs: &prefs,
            contract_type: "art",
            creator_address: "0xartist",
            created_at: &chrono::Utc::now().to_rfc3339(),
            features: &None,
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };

        let (score, _) = strategy.score(&ctx);
        assert!(score > 0.0, "high content_type weight should still produce positive score");
        assert!((0.0..=1.0).contains(&score), "score must stay in [0, 1]");
    }

    // ── FollowingScoring strategy ─────────────────────────────────────────────

    #[test]
    fn following_scoring_always_returns_following_reason() {
        let strategy = FollowingScoring;
        let prefs = UserPreferences::default();
        let ctx = ScoringContext {
            prefs: &prefs,
            contract_type: "snap",
            creator_address: "0xfollowee",
            created_at: &chrono::Utc::now().to_rfc3339(),
            features: &None,
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };

        let (_, reason) = strategy.score(&ctx);
        match reason {
            RecommendationReason::Following { followee } => {
                assert_eq!(followee, "0xfollowee");
            }
            _ => panic!("FollowingScoring must always return Following reason, got {reason:?}"),
        }
    }

    #[test]
    fn following_scoring_recent_content_scores_higher_than_old() {
        let strategy = FollowingScoring;
        let prefs = UserPreferences::default();
        let features = make_features(vec![], 0.5, 0.0, 0.0);

        let now = chrono::Utc::now();
        let recent_ts = now.to_rfc3339();
        let old_ts = (now - chrono::Duration::days(30)).to_rfc3339();

        let ctx_recent = ScoringContext {
            prefs: &prefs,
            contract_type: "snap",
            creator_address: "0xcreator",
            created_at: &recent_ts,
            features: &Some(features.clone()),
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };
        let ctx_old = ScoringContext {
            prefs: &prefs,
            contract_type: "snap",
            creator_address: "0xcreator",
            created_at: &old_ts,
            features: &Some(features),
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };

        let (recent_score, _) = strategy.score(&ctx_recent);
        let (old_score, _) = strategy.score(&ctx_old);

        assert!(
            recent_score > old_score,
            "FollowingScoring must rank recent content higher: recent={recent_score} old={old_score}"
        );
    }

    #[test]
    fn following_scoring_high_engagement_raises_score() {
        let strategy = FollowingScoring;
        let prefs = UserPreferences::default();
        let ts = chrono::Utc::now().to_rfc3339();

        let low_eng = make_features(vec![], 0.1, 0.0, 0.0);
        let high_eng = make_features(vec![], 0.9, 0.0, 0.0);

        let ctx_low = ScoringContext {
            prefs: &prefs,
            contract_type: "snap",
            creator_address: "0xcreator",
            created_at: &ts,
            features: &Some(low_eng),
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };
        let ctx_high = ScoringContext {
            prefs: &prefs,
            contract_type: "snap",
            creator_address: "0xcreator",
            created_at: &ts,
            features: &Some(high_eng),
            seen_creators: &HashMap::new(),
            seen_tags: &HashMap::new(),
        };

        let (score_low, _) = strategy.score(&ctx_low);
        let (score_high, _) = strategy.score(&ctx_high);

        assert!(
            score_high > score_low,
            "higher engagement should raise FollowingScoring score: high={score_high} low={score_low}"
        );
    }

    // ── score_single_nft ──────────────────────────────────────────────────────

    #[test]
    fn score_single_nft_returns_some_for_valid_nft() {
        let prefs = UserPreferences::default();
        let strategy = WeightedScoring::default();
        let nft = make_candidate("abc123", "0xcreator", "art");
        let features = make_features(vec!["abstract"], 0.5, 0.3, 0.7);

        let result = score_single_nft(
            &nft,
            &Some(features),
            &prefs,
            &strategy,
            &HashMap::new(),
            &HashMap::new(),
        );

        let scored = result.expect("should return Some for a valid nft");
        assert_eq!(scored.nft_id, "abc123");
        assert_eq!(scored.creator_address, "0xcreator");
        assert!((0.0..=1.0).contains(&scored.score));
    }

    #[test]
    fn score_single_nft_returns_none_for_missing_id() {
        let prefs = UserPreferences::default();
        let strategy = WeightedScoring::default();
        let mut nft = make_candidate("ignored", "0xcreator", "art");
        nft.id = None; // skip sentinel

        let result =
            score_single_nft(&nft, &None, &prefs, &strategy, &HashMap::new(), &HashMap::new());

        assert!(result.is_none(), "missing id should return None");
    }

    #[test]
    fn score_single_nft_tags_propagated_from_features() {
        let prefs = UserPreferences::default();
        let strategy = WeightedScoring::default();
        let nft = make_candidate("xyz", "0xcreator", "music");
        let features = make_features(vec!["jazz", "soul"], 0.0, 0.0, 0.0);

        let scored = score_single_nft(
            &nft,
            &Some(features),
            &prefs,
            &strategy,
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("should return Some");

        assert!(scored.tags.contains(&"jazz".to_string()));
        assert!(scored.tags.contains(&"soul".to_string()));
    }

    // ── score_batch ───────────────────────────────────────────────────────────

    #[test]
    fn score_batch_skips_nfts_with_missing_id() {
        let prefs = UserPreferences::default();
        let strategy = WeightedScoring::default();

        let mut no_id = make_candidate("skip", "0xcreator", "art");
        no_id.id = None;
        let valid = make_candidate("keep", "0xcreator", "art");

        let candidates = vec![(no_id, None), (valid, None)];
        let mut seen_c = HashMap::new();
        let mut seen_t = HashMap::new();

        let results = score_batch(candidates, &prefs, &strategy, &mut seen_c, &mut seen_t);
        assert_eq!(results.len(), 1, "only the nft with an id should be scored");
        assert_eq!(results[0].nft_id, "keep");
    }

    #[test]
    fn score_batch_updates_seen_creators_map() {
        let prefs = UserPreferences::default();
        let strategy = WeightedScoring::default();

        let c1 = make_candidate("n1", "0xalice", "art");
        let c2 = make_candidate("n2", "0xalice", "art");
        let c3 = make_candidate("n3", "0xbob", "snap");

        let candidates = vec![(c1, None), (c2, None), (c3, None)];
        let mut seen_c: HashMap<String, usize> = HashMap::new();
        let mut seen_t = HashMap::new();

        score_batch(candidates, &prefs, &strategy, &mut seen_c, &mut seen_t);

        assert_eq!(
            seen_c.get("0xalice").copied().unwrap_or(0),
            2,
            "0xalice appears twice so seen_creators should count 2"
        );
        assert_eq!(
            seen_c.get("0xbob").copied().unwrap_or(0),
            1
        );
    }

    // ── nan_safe_sort_desc ────────────────────────────────────────────────────

    #[test]
    fn nan_safe_sort_desc_sorts_descending() {
        let make = |id: &str, s: f32| ScoredNft {
            nft_id: id.to_string(),
            token_id: 1,
            contract_address: "0x".to_string(),
            score: s,
            reason: RecommendationReason::Discovery,
            contract_type: "art".to_string(),
            creator_address: "0xc".to_string(),
            tags: vec![],
        };

        let mut items = vec![make("a", 0.3), make("b", 0.9), make("c", 0.5)];
        nan_safe_sort_desc(&mut items);

        assert_eq!(items[0].score, 0.9);
        assert_eq!(items[1].score, 0.5);
        assert_eq!(items[2].score, 0.3);
    }

    #[test]
    fn nan_safe_sort_desc_nan_scores_sort_last() {
        let make = |id: &str, s: f32| ScoredNft {
            nft_id: id.to_string(),
            token_id: 1,
            contract_address: "0x".to_string(),
            score: s,
            reason: RecommendationReason::Discovery,
            contract_type: "art".to_string(),
            creator_address: "0xc".to_string(),
            tags: vec![],
        };

        let mut items = vec![make("nan", f32::NAN), make("ok", 0.7), make("zero", 0.0)];
        nan_safe_sort_desc(&mut items);

        assert!(!items[0].score.is_nan(), "first item must not be NaN");
        assert!(items.last().unwrap().score.is_nan(), "NaN must sort last");
    }

    // ── compute_recency_score ─────────────────────────────────────────────────

    #[test]
    fn recency_score_recent_beats_old() {
        let now = chrono::Utc::now();
        let recent = now.to_rfc3339();
        let old = (now - chrono::Duration::days(10)).to_rfc3339();

        let r_recent = compute_recency_score(&recent);
        let r_old = compute_recency_score(&old);
        assert!(r_recent > r_old, "recent content should score higher: {r_recent} vs {r_old}");
    }

    #[test]
    fn recency_score_invalid_timestamp_returns_fallback() {
        let score = compute_recency_score("not-a-date");
        assert_eq!(score, 0.5, "invalid timestamp should return 0.5 fallback");
    }

    #[test]
    fn recency_score_very_recent_approaches_one() {
        let just_now = chrono::Utc::now().to_rfc3339();
        let score = compute_recency_score(&just_now);
        // exp(0) = 1.0; a few milliseconds ago should be very close to 1
        assert!(score > 0.99, "brand-new content should score near 1.0, got {score}");
    }

    #[test]
    fn recency_score_very_old_approaches_zero() {
        let ancient = "2000-01-01T00:00:00Z";
        let score = compute_recency_score(ancient);
        assert!(score < 0.01, "ancient content should score near 0.0, got {score}");
    }

    // ── apply_diversity_shuffle_static ────────────────────────────────────────

    #[test]
    fn diversity_shuffle_returns_all_when_below_limit() {
        let make_scored = |id: &str| ScoredNft {
            nft_id: id.to_string(),
            token_id: 1,
            contract_address: "0x".to_string(),
            score: 0.5,
            reason: RecommendationReason::Discovery,
            contract_type: "art".to_string(),
            creator_address: "0xc".to_string(),
            tags: vec![],
        };

        let items: Vec<_> = (0..5).map(|i| make_scored(&i.to_string())).collect();
        let result = apply_diversity_shuffle_static(items, 10);
        assert_eq!(result.len(), 5, "when input < limit, all items should be returned");
    }

    #[test]
    fn diversity_shuffle_truncates_to_limit() {
        let types = ["snap", "art", "music", "flix"];
        let make_scored = |id: usize| ScoredNft {
            nft_id: id.to_string(),
            token_id: 1,
            contract_address: "0x".to_string(),
            score: 0.5,
            reason: RecommendationReason::Discovery,
            // Rotate across all four content types so no single type hits the 60% cap
            contract_type: types[id % 4].to_string(),
            creator_address: format!("0x{:040x}", id),
            tags: vec![],
        };

        let items: Vec<_> = (0..100).map(make_scored).collect();
        let result = apply_diversity_shuffle_static(items, 20);
        assert_eq!(result.len(), 20, "result should be capped at limit");
    }
}

#[cfg(test)]
mod session_boost_tests {
    use super::*;

    fn make_signal(tags: Vec<&str>, creator: Option<&str>, weight: f32, age_secs: i64) -> SessionSignal {
        SessionSignal {
            tags: tags.into_iter().map(str::to_string).collect(),
            creator: creator.map(str::to_string),
            interaction_weight: weight,
            ts_unix: 1_000_000 - age_secs,
        }
    }

    #[test]
    fn fresh_signal_full_weight() {
        let signal = make_signal(vec!["art"], Some("0xcreator"), 1.0, 0);
        let (tb, cb) = compute_session_boost_maps(&[signal], 1_000_000);
        let art = tb["art"];
        assert!((art - 1.0).abs() < 1e-5, "fresh art boost should be ~1.0, got {art}");
        let c = cb["0xcreator"];
        assert!((c - 1.0).abs() < 1e-5, "fresh creator boost should be ~1.0, got {c}");
    }

    #[test]
    fn signal_at_1800s_exp_decay() {
        let signal = make_signal(vec!["music"], None, 1.0, 1800);
        let (tb, _) = compute_session_boost_maps(&[signal], 1_000_000);
        let expected = (-1.0_f32).exp();
        let got = tb["music"];
        assert!((got - expected).abs() < 1e-5, "at 1800s: expected {expected:.4}, got {got:.4}");
    }

    #[test]
    fn old_signal_near_zero() {
        let signal = make_signal(vec!["photography"], None, 1.0, 21_600);
        let (tb, _) = compute_session_boost_maps(&[signal], 1_000_000);
        let got = tb["photography"];
        assert!(got < 0.001, "6h signal should be near-zero, got {got}");
    }

    #[test]
    fn accumulation_clamps_at_1() {
        let s1 = make_signal(vec!["art"], None, 1.0, 0);
        let s2 = make_signal(vec!["art"], None, 1.0, 0);
        let (tb, _) = compute_session_boost_maps(&[s1, s2], 1_000_000);
        let art = tb["art"];
        assert!((art - 1.0).abs() < 1e-5, "sum 2.0 must clamp to 1.0, got {art}");
    }

    #[test]
    fn lowercases_tags() {
        let signal = make_signal(vec!["StreetArt", "ABSTRACT"], None, 0.5, 0);
        let (tb, _) = compute_session_boost_maps(&[signal], 1_000_000);
        assert!(tb.contains_key("streetart"), "tags must be lowercased");
        assert!(tb.contains_key("abstract"), "tags must be lowercased");
        assert!(!tb.contains_key("StreetArt"), "mixed-case key must not appear");
    }

    #[test]
    fn empty_signals_empty_maps() {
        let (tb, cb) = compute_session_boost_maps(&[], 1_000_000);
        assert!(tb.is_empty());
        assert!(cb.is_empty());
    }
}
