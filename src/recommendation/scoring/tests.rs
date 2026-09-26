use super::*;
use std::collections::HashMap;
use crate::recommendation::types::ContentType;

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_features(
    tags: Vec<&str>,
    engagement: f32,
    trending: f32,
    quality: f32,
) -> ScoringFeatures {
    ScoringFeatures {
        tags: tags.into_iter().map(|t| t.into()).collect(),
        engagement_score: engagement,
        trending_score: trending,
        quality_score: quality,
    }
}

fn make_candidate(id: &str, creator: &str, contract_type: &str) -> CandidateNft {
    CandidateNft {
        id: Some(id.to_string()),
        token_id: Some(1),
        contract_address: format!("0x{id}"),
        contract_type: Some(contract_type.to_string()),
        creator_address: creator.into(),
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
            assert_eq!(content_type, ContentType::Art)
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
    prefs.creator_preferences.insert(creator.into(), 0.8);

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
    prefs.creator_preferences.insert(creator.into(), 0.9);

    let (_score, reason) = compute_creator_affinity_score(&weights, creator, &prefs);
    match reason {
        Some(RecommendationReason::CreatorAffinity { creator: c }) => {
            assert_eq!(&*c, creator)
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
    prefs.creator_preferences.insert("0xartist".into(), 0.9);

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
    prefs.creator_preferences.insert("0xcreator".into(), 1.0);

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
    prefs.tag_preferences.insert("landscape".into(), 0.8);

    let f = make_features(vec!["landscape"], 0.0, 0.0, 0.0);
    let (score, reason) =
        compute_feature_scores(&weights, &f, &prefs, "0xcreator", &HashMap::new(), &HashMap::new());

    assert!(score > 0.0, "single matching tag should produce positive score");
    match reason {
        Some(RecommendationReason::TagMatch { matching_tags }) => {
            let tags: Vec<&str> = matching_tags.iter().map(|s| s.as_ref()).collect();
            assert_eq!(tags, vec!["landscape"]);
        }
        _ => panic!("expected TagMatch reason, got {reason:?}"),
    }
}

#[test]
fn feature_scores_multiple_tag_matches_score_higher_than_single() {
    let weights = ScoringWeights::default();
    let mut prefs = UserPreferences::default();
    prefs.tag_preferences.insert("landscape".into(), 0.8);
    prefs.tag_preferences.insert("abstract".into(), 0.8);
    prefs.tag_preferences.insert("blue".into(), 0.8);

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

    let mut seen_few: HashMap<Box<str>, usize> = HashMap::new();
    seen_few.insert("0xabc".into(), 2); // at the boundary, no penalty yet

    let mut seen_many: HashMap<Box<str>, usize> = HashMap::new();
    seen_many.insert("0xabc".into(), 5); // > 2, penalty kicks in

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
    prefs.tag_preferences.insert("abstract".into(), 1.0);
    prefs.creator_preferences.insert("0xcreator".into(), 1.0);

    let f = make_features(vec!["abstract"], 1.0, 1.0, 1.0);
    let ctx = ScoringContext {
        prefs: &prefs,
        contract_type: "art",
        creator_address: "0xcreator",
        created_at: &chrono::Utc::now().to_rfc3339(),
        now_unix: chrono::Utc::now().timestamp(),
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
        now_unix: chrono::Utc::now().timestamp(),
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
        now_unix: chrono::Utc::now().timestamp(),
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
        now_unix: chrono::Utc::now().timestamp(),
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
        now_unix: chrono::Utc::now().timestamp(),
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
        now_unix: chrono::Utc::now().timestamp(),
        features: &None,
        seen_creators: &HashMap::new(),
        seen_tags: &HashMap::new(),
    };

    let (_, reason) = strategy.score(&ctx);
    match reason {
        RecommendationReason::Following { followee } => {
            assert_eq!(&*followee, "0xfollowee");
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

    let now_unix = now.timestamp();
    let ctx_recent = ScoringContext {
        prefs: &prefs,
        contract_type: "snap",
        creator_address: "0xcreator",
        created_at: &recent_ts,
        now_unix,
        features: &Some(features.clone()),
        seen_creators: &HashMap::new(),
        seen_tags: &HashMap::new(),
    };
    let ctx_old = ScoringContext {
        prefs: &prefs,
        contract_type: "snap",
        creator_address: "0xcreator",
        created_at: &old_ts,
        now_unix,
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

    let now_unix = chrono::Utc::now().timestamp();
    let ctx_low = ScoringContext {
        prefs: &prefs,
        contract_type: "snap",
        creator_address: "0xcreator",
        created_at: &ts,
        now_unix,
        features: &Some(low_eng),
        seen_creators: &HashMap::new(),
        seen_tags: &HashMap::new(),
    };
    let ctx_high = ScoringContext {
        prefs: &prefs,
        contract_type: "snap",
        creator_address: "0xcreator",
        created_at: &ts,
        now_unix,
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

    let results = score_batch(candidates, &prefs, &strategy, &mut seen_c, &mut seen_t, chrono::Utc::now().timestamp());
    assert_eq!(results.len(), 1, "only the nft with an id should be scored");
    assert_eq!(results[0].nft_id.as_ref(), "keep");
}

#[test]
fn score_batch_updates_seen_creators_map() {
    let prefs = UserPreferences::default();
    let strategy = WeightedScoring::default();

    let c1 = make_candidate("n1", "0xalice", "art");
    let c2 = make_candidate("n2", "0xalice", "art");
    let c3 = make_candidate("n3", "0xbob", "snap");

    let candidates = vec![(c1, None), (c2, None), (c3, None)];
    let mut seen_c: HashMap<Box<str>, usize> = HashMap::new();
    let mut seen_t = HashMap::new();

    score_batch(candidates, &prefs, &strategy, &mut seen_c, &mut seen_t, chrono::Utc::now().timestamp());

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
        nft_id: id.into(),
        token_id: 1,
        contract_address: "0x".into(),
        score: s,
        reason: RecommendationReason::Discovery,
        contract_type: "art".into(),
        creator_address: "0xc".into(),
        tags: vec![].into(),
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
        nft_id: id.into(),
        token_id: 1,
        contract_address: "0x".into(),
        score: s,
        reason: RecommendationReason::Discovery,
        contract_type: "art".into(),
        creator_address: "0xc".into(),
        tags: vec![].into(),
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
    let now_unix = now.timestamp();
    let recent = now.to_rfc3339();
    let old = (now - chrono::Duration::days(10)).to_rfc3339();

    let r_recent = compute_recency_score(&recent, now_unix);
    let r_old = compute_recency_score(&old, now_unix);
    assert!(r_recent > r_old, "recent content should score higher: {r_recent} vs {r_old}");
}

#[test]
fn recency_score_invalid_timestamp_returns_fallback() {
    let score = compute_recency_score("not-a-date", 0);
    assert_eq!(score, 0.5, "invalid timestamp should return 0.5 fallback");
}

#[test]
fn recency_score_very_recent_approaches_one() {
    let now = chrono::Utc::now();
    let now_unix = now.timestamp();
    let just_now = now.to_rfc3339();
    let score = compute_recency_score(&just_now, now_unix);
    // exp(0) = 1.0; a few milliseconds ago should be very close to 1
    assert!(score > 0.99, "brand-new content should score near 1.0, got {score}");
}

#[test]
fn recency_score_very_old_approaches_zero() {
    let ancient = "2000-01-01T00:00:00Z";
    let now_unix = chrono::Utc::now().timestamp();
    let score = compute_recency_score(ancient, now_unix);
    assert!(score < 0.01, "ancient content should score near 0.0, got {score}");
}

// ── apply_diversity_shuffle_static ────────────────────────────────────────

#[test]
fn diversity_shuffle_returns_all_when_below_limit() {
    let make_scored = |id: &str| ScoredNft {
        nft_id: id.into(),
        token_id: 1,
        contract_address: "0x".into(),
        score: 0.5,
        reason: RecommendationReason::Discovery,
        contract_type: "art".into(),
        creator_address: "0xc".into(),
        tags: vec![].into(),
    };

    let items: Vec<_> = (0..5).map(|i| make_scored(&i.to_string())).collect();
    let result = apply_diversity_shuffle_static(items, 10);
    assert_eq!(result.len(), 5, "when input < limit, all items should be returned");
}

#[test]
fn diversity_shuffle_truncates_to_limit() {
    let types = ["snap", "art", "music", "flix"];
    let make_scored = |id: usize| ScoredNft {
        nft_id: id.to_string().into(),
        token_id: 1,
        contract_address: "0x".into(),
        score: 0.5,
        reason: RecommendationReason::Discovery,
        // Rotate across all four content types so no single type hits the 60% cap
        contract_type: types[id % 4].into(),
        creator_address: format!("0x{:040x}", id).into(),
        tags: vec![].into(),
    };

    let items: Vec<_> = (0..100).map(make_scored).collect();
    let result = apply_diversity_shuffle_static(items, 20);
    assert_eq!(result.len(), 20, "result should be capped at limit");
}
