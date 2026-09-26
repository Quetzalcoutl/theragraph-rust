use std::collections::HashMap;
use std::sync::Arc;

use crate::recommendation::{
    cache::SessionSignal,
    candidate_repository::CandidateNft,
    features::ScoringFeatures,
    preferences::UserPreferences,
    types::ContentType,
};

use super::{RecommendationReason, ScoredNft, ScoringContext, ScoringStrategy, ScoringWeights};

// ── Free scoring helpers ──────────────────────────────────────────────────────

/// Sort `items` by score descending, NaN scores sort last.
pub(crate) fn nan_safe_sort_desc(items: &mut Vec<ScoredNft>) {
    items.sort_unstable_by(|a, b| match (a.score.is_nan(), b.score.is_nan()) {
        (true, _) => std::cmp::Ordering::Greater,
        (_, true) => std::cmp::Ordering::Less,
        _ => b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal),
    });
}

/// ByteGraph-inspired content type affinity scoring with dynamic boosting.
/// Uses the user's actual affinity values directly (already normalized 0-1).
pub(crate) fn compute_type_affinity_score(
    weights: &ScoringWeights,
    contract_type: &str,
    prefs: &UserPreferences,
) -> (f32, Option<RecommendationReason>) {
    // J1/J2: parse once, reuse for both affinity lookup and reason.
    // ContentType is Copy so no clone needed for the two uses below.
    // from_str byte-matches lowercase (hot-path invariant) without allocating.
    let ct: Option<ContentType> = ContentType::from_str(contract_type);
    let type_affinity = ct.map(|c| prefs.affinity_for(c)).unwrap_or(0.5);

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
    // ct is Copy — reused here without clone; moves into ContentTypeMatch.
    let reason = if type_affinity > 0.55 {
        ct.map(|c| RecommendationReason::ContentTypeMatch { content_type: c })
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
            creator: creator.into(),
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
    seen_creators: &HashMap<Box<str>, usize>,
    seen_tags: &HashMap<Arc<str>, usize>,
) -> (f32, Option<RecommendationReason>) {
    let mut total = 0.0f32;
    // Tracks (best_score, best_reason) together — no separate max_score proxy.
    let mut primary: Option<(f32, RecommendationReason)> = None;

    // Dynamic threshold based on user's tag diversity
    let tag_threshold = if prefs.tag_preferences.len() > 20 { 0.65 } else { 0.6 };

    // EFF-002: tags are stored lowercase; no to_lowercase() allocation needed.
    // I2: don't push clones during scan — only the primary-winner slot needs the Vec.
    // For ~90% of NFTs (where tags aren't the top-scoring dimension), this saves
    // 1 Vec alloc + 1–10 String clones per NFT across 500 candidates.
    let mut tag_score_sum = 0.0f32;
    let mut match_count = 0usize;

    for tag in f.tags.iter() {
        let pref = prefs
            .tag_preferences
            .get(tag.as_ref())
            .copied()
            .unwrap_or(0.3);
        if pref > tag_threshold {
            tag_score_sum += pref;
            match_count += 1;
        }
    }

    // Exponential boost for multiple tag matches (ByteGraph collaborative signal)
    let tag_match_score = if match_count > 0 {
        let base_score = tag_score_sum / match_count as f32;
        // Apply exponential boost: 1 match = 1x, 2 matches = 1.3x, 3+ matches = 1.6x
        let match_multiplier = 1.0 + (match_count as f32 - 1.0) * 0.15;
        base_score * weights.tag_match * match_multiplier.min(1.6)
    } else {
        // Penalty for NFTs with no tag overlap
        -0.1 * weights.tag_match
    };
    total += tag_match_score;

    if tag_match_score > 0.0 && match_count > 0 {
        if primary.as_ref().map_or(true, |(s, _)| tag_match_score > *s) {
            // Materialize only when this is the winning dimension.
            let matching_tags: Box<[Arc<str>]> = f.tags.iter()
                .filter(|tag| prefs.tag_preferences.get(tag.as_ref()).copied().unwrap_or(0.3) > tag_threshold)
                .cloned()
                .collect();
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
        .map(|t| seen_tags.get(t.as_ref()).copied().unwrap_or(0) as f32)
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

    // Owned array so `for (score, reason) in signal_pairs` moves each element —
    // the winning reason is moved into `primary_reason`, never cloned.
    let signal_pairs = [
        compute_type_affinity_score(weights, ctx.contract_type, ctx.prefs),
        compute_creator_affinity_score(weights, ctx.creator_address, ctx.prefs),
        feature_pair.unwrap_or((0.0, None)),
    ];

    let mut total = 0.0f32;
    let mut winner: Option<(f32, RecommendationReason)> = None;

    for (sig_score, sig_reason) in signal_pairs {
        total += sig_score;
        if let Some(r) = sig_reason {
            if winner.as_ref().map_or(true, |(s, _)| sig_score > *s) {
                winner = Some((sig_score, r));
            }
        }
    }
    let primary_reason = winner.map(|(_, r)| r).unwrap_or(RecommendationReason::Discovery);

    // Recency bonus (no associated reason — it is never the primary signal)
    total += compute_recency_score(ctx.created_at, ctx.now_unix) * weights.recency;

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
///
/// `now_unix` is the caller's pre-computed Unix epoch (seconds), captured once
/// before the Rayon batch — avoids one vDSO clock_gettime syscall per NFT.
pub(crate) fn compute_recency_score(created_at: &str, now_unix: i64) -> f32 {
    match chrono::DateTime::parse_from_rfc3339(created_at) {
        Ok(dt) => {
            // TG-01: clamp age to ≥ 0 so a future-dated timestamp (clock skew or
            // data error) does not produce a negative exponent argument that makes
            // exp() return a value > 1.0 (= f32::INFINITY for very far-future dates),
            // which then breaks JSON serialization and biases ranking.
            let age_secs = (now_unix - dt.timestamp()).max(0) as f32;
            let age_hours = age_secs / 3600.0;
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
    use rand::seq::IndexedRandom;

    if scored.len() <= limit {
        return scored;
    }

    // Take top 80% deterministically, shuffle remaining 20% slots
    let deterministic_count = (limit as f32 * 0.8) as usize;
    let shuffle_count = limit - deterministic_count;

    let mut result: Vec<ScoredNft> = scored.drain(..deterministic_count).collect();

    // From remaining, pick some randomly for discovery
    let mut rng = rand::rng();
    let remaining: Vec<_> = scored.into_iter().take(shuffle_count * 3).collect();

    if !remaining.is_empty() {
        let chosen: Vec<_> = remaining
            .sample(&mut rng, shuffle_count.min(remaining.len()))
            .cloned()
            .collect();
        result.extend(chosen);
    }

    // Content-type cap: no single type may exceed 60% of the final `limit` slots.
    // Applied as a post-filter so the highest-scored items of each type survive;
    // overflow items are dropped rather than reordered.
    //
    // I4: 4-slot array replaces HashMap<String,usize> — eliminates contract_type.clone()
    // on every item (50 clones/feed) and HashMap hashing overhead. Index: snap=0, art=1,
    // music=2, flix/unknown=3.
    let max_per_type = ((limit as f32 * 0.60).ceil() as usize).max(1);
    let mut type_counts = [0usize; 4];
    result.retain(|item| {
        let idx = match item.contract_type.as_ref() {
            "snap"  => 0,
            "art"   => 1,
            "music" => 2,
            _       => 3,
        };
        if type_counts[idx] < max_per_type {
            type_counts[idx] += 1;
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
) -> (HashMap<Box<str>, f32>, HashMap<Box<str>, f32>) {
    let mut tag_boosts: HashMap<Box<str>, f32> = HashMap::with_capacity(signals.len() * 5);
    let mut creator_boosts: HashMap<Box<str>, f32> = HashMap::with_capacity(signals.len());

    for sig in signals {
        let age_secs = (now_unix - sig.ts_unix).max(0) as f32;
        // 30-min half-life: exp(-age / 1800). At 0 min → 1.0, at 30 min → 0.5, at 2h → 0.09.
        let decay = (-age_secs / 1800.0_f32).exp();
        let decayed = sig.interaction_weight * decay;

        for tag in &sig.tags {
            *tag_boosts.entry(tag.to_lowercase().into()).or_insert(0.0) += decayed;
        }
        if let Some(ref creator) = sig.creator {
            // EFF-002/VID-CASE-001: nft_creator_address pre-normalized to lowercase at every
            // write path. No to_lowercase() alloc needed here.
            *creator_boosts.entry(creator.clone()).or_insert(0.0) += decayed;
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
/// `now_unix` is Unix epoch seconds pre-computed by the caller so
/// `compute_recency_score` avoids a vDSO syscall per NFT.
/// Pure function — no DB, no async, no side effects. Callers own the
/// seen-creator/tag maps; pass empty maps when diversity tracking is not
/// needed (e.g. parallel chunks that later merge).
pub(crate) fn score_batch(
    candidates: Vec<(CandidateNft, Option<ScoringFeatures>)>,
    prefs: &UserPreferences,
    strategy: &dyn ScoringStrategy,
    seen_creators: &mut HashMap<Box<str>, usize>,
    seen_tags: &mut HashMap<Arc<str>, usize>,
    now_unix: i64,
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
        // Draft (not-yet-minted) content has no on-chain token_id yet — 0 is
        // the same sentinel the frontend's isDraftNft(tokenId) already uses
        // (a real minted token_id is never 0).
        let token_id = token_id.unwrap_or(0);
        let contract_type = contract_type.unwrap_or_default();
        let created_at    = created_at.as_deref().unwrap_or("");

        let ctx = ScoringContext {
            prefs,
            contract_type: &contract_type,
            creator_address: &creator_address,
            created_at,
            now_unix,
            features: &features,
            seen_creators,
            seen_tags,
        };

        let (score, reason) = strategy.score(&ctx);

        // RS-06: avoid cloning the key string when the entry already exists —
        // most candidates share creators/tags so the majority of clones were discarded.
        // EFF-003: for new creators, one clone is unavoidable (HashMap needs owned key);
        // then creator_address itself is moved into ScoredNft — net saving vs before.
        if let Some(count) = seen_creators.get_mut(creator_address.as_str()) {
            *count += 1;
        } else {
            seen_creators.insert(creator_address.as_str().into(), 1);
        }
        if let Some(tags) = features.as_ref().map(|f| &f.tags) {
            for tag in tags {
                // tag: &Box<str>; get_mut via Borrow<str> fast path
                if let Some(count) = seen_tags.get_mut(tag.as_ref()) {
                    *count += 1;
                } else {
                    seen_tags.insert(tag.clone(), 1);
                }
            }
        }

        scored.push(ScoredNft {
            nft_id: nft_id.into(),
            token_id,
            contract_address: contract_address.into(),
            score,
            reason,
            contract_type: contract_type.into(),
            creator_address: creator_address.into(),
            tags: features.map(|f| f.tags).unwrap_or_default(),
        });
    }

    scored
}
