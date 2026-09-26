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
        pct_played: None,
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
    prefs.tag_preferences.insert(tag.as_str().into(), 0.99);

    for _ in 0..50 {
        let event = make_event(InteractionType::Like, None, None, vec!["photography"], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());
    }

    let value = prefs.tag_preferences.get(tag.as_str()).copied().unwrap_or(0.5);
    assert!(
        value <= 1.0,
        "tag preference must not exceed 1.0; got {value}"
    );
}

#[test]
fn tag_preference_is_clamped_to_0_0_after_many_negative_interactions() {
    let mut prefs = UserPreferences::default();
    let tag = "abstract".to_string();
    prefs.tag_preferences.insert(tag.as_str().into(), 0.01);

    for _ in 0..50 {
        let event = make_event(InteractionType::Unlike, None, None, vec!["abstract"], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());
    }

    let value = prefs.tag_preferences.get(tag.as_str()).copied().unwrap_or(0.5);
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
    let mut map: HashMap<Box<str>, f32> = HashMap::new();
    map.insert("a".into(), 0.1);
    map.insert("b".into(), 0.9);
    // max_size == map.len() → no eviction
    evict(&mut map, 2, EvictionPolicy::LowestWeight);
    assert_eq!(map.len(), 2, "map should be unchanged when len <= max_size");
    assert!(map.contains_key("a"));
    assert!(map.contains_key("b"));
}

#[test]
fn evict_lowest_weight_removes_min_entry() {
    let mut map: HashMap<Box<str>, f32> = HashMap::new();
    map.insert("high".into(), 0.9);
    map.insert("low".into(), 0.1);
    map.insert("mid".into(), 0.5);
    // map.len() = 3 > max_size = 2 → should remove "low"
    evict(&mut map, 2, EvictionPolicy::LowestWeight);
    assert_eq!(map.len(), 2, "one entry should have been evicted");
    assert!(!map.contains_key("low"), "the lowest-weight entry must be removed");
    assert!(map.contains_key("high"));
    assert!(map.contains_key("mid"));
}

#[test]
fn evict_max_size_zero_removes_sole_entry() {
    let mut map: HashMap<Box<str>, f32> = HashMap::new();
    map.insert("only".into(), 0.5);
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
    prefs.tag_preferences.insert(tag.into(), before);

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

// -----------------------------------------------------------------------
// NotInterested: creator AND tag fast-suppression parity
// -----------------------------------------------------------------------

#[test]
fn not_interested_suppresses_creator_to_10_percent() {
    let mut prefs = UserPreferences::default();
    prefs.creator_preferences.insert("0xcreator".into(), 0.8);

    let event = make_event(InteractionType::NotInterested, None, Some("0xcreator"), vec![], None);
    apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

    // Generic loop applies the NOT_INTERESTED_WEIGHT*CREATOR_DELTA_FACTOR delta
    // first (0.8 - 0.15 = 0.65), THEN the match-arm override suppresses to 10%
    // of that post-delta value: max(0.65 * 0.1, 0.05) = 0.065.
    let after = prefs.creator_preferences.get("0xcreator").copied().unwrap();
    assert!((after - 0.065).abs() < 1e-5, "expected 0.065, got {after}");
}

#[test]
fn not_interested_suppresses_tags_to_10_percent_same_as_creator() {
    let mut prefs = UserPreferences::default();
    prefs.tag_preferences.insert("cyberpunk".into(), 0.9);

    let event = make_event(InteractionType::NotInterested, None, None, vec!["cyberpunk"], None);
    apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

    // Generic loop: 0.9 - 0.15 = 0.75. Override: max(0.75 * 0.1, 0.05) = 0.075.
    let after = prefs.tag_preferences.get("cyberpunk").copied().unwrap();
    assert!((after - 0.075).abs() < 1e-5, "expected 0.075 (parity with creator suppression math), got {after}");
}

#[test]
fn not_interested_suppresses_tags_to_minimum_005_when_current_is_low() {
    let mut prefs = UserPreferences::default();
    prefs.tag_preferences.insert("abstract".into(), 0.2);

    let event = make_event(InteractionType::NotInterested, None, None, vec!["abstract"], None);
    apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

    // 10% of 0.2 = 0.02, floored to the 0.05 minimum.
    let after = prefs.tag_preferences.get("abstract").copied().unwrap();
    assert!((after - 0.05).abs() < 1e-5, "expected floor of 0.05, got {after}");
}

#[test]
fn not_interested_suppresses_multiple_tags_independently() {
    let mut prefs = UserPreferences::default();
    prefs.tag_preferences.insert("dark".into(), 0.7);
    prefs.tag_preferences.insert("neon".into(), 0.4);

    let event = make_event(InteractionType::NotInterested, None, None, vec!["dark", "neon"], None);
    apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

    // dark: 0.7 - 0.15 = 0.55, then max(0.55*0.1, 0.05) = 0.055.
    assert!((prefs.tag_preferences.get("dark").copied().unwrap() - 0.055).abs() < 1e-5);
    // neon: 0.4 - 0.15 = 0.25, then max(0.25*0.1, 0.05) = 0.05 (floored).
    assert!((prefs.tag_preferences.get("neon").copied().unwrap() - 0.05).abs() < 1e-5);
}

#[test]
fn not_interested_on_a_never_seen_tag_starts_from_default_0_5() {
    let mut prefs = UserPreferences::default();
    assert!(prefs.tag_preferences.get("brandnew").is_none());

    let event = make_event(InteractionType::NotInterested, None, None, vec!["brandnew"], None);
    apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());

    // Generic loop first inserts 0.5 + NOT_INTERESTED_WEIGHT*TAG_DELTA_FACTOR = 0.5 - 0.15 = 0.35,
    // then the fast-suppression override forces it to 10% of THAT (0.035), floored to 0.05.
    let after = prefs.tag_preferences.get("brandnew").copied().unwrap();
    assert!((after - 0.05).abs() < 1e-5, "expected 0.05 floor, got {after}");
}

// -----------------------------------------------------------------------
// apply_interaction_to_prefs — replay determinism
//
// The rebuild/insurance-policy job (recommendation::recorder::rebuild)
// reconstructs a user's whole UserPreferences row by folding their
// user_interactions history through this same function from a fresh
// Default::default() state. That only recovers the original values if
// folding the same event sequence twice, from the same starting point,
// always produces the same result — i.e. the function is a pure,
// deterministic fold with no hidden time- or order-of-HashMap-iteration
// dependence for a fixed input sequence.
// -----------------------------------------------------------------------

#[test]
fn replaying_the_same_event_sequence_twice_produces_identical_preferences() {
    let events = vec![
        make_event(InteractionType::Like, Some("music"), Some("0xcreator1"), vec!["psy-trance", "ambient"], None),
        make_event(InteractionType::View, Some("flix"), Some("0xcreator2"), vec!["psy-trance", "documentary"], Some(5_000)),
        make_event(InteractionType::Comment, Some("art"), Some("0xcreator1"), vec!["psychedelic"], None),
        make_event(InteractionType::NotInterested, None, Some("0xcreator3"), vec!["ambient"], None),
        make_event(InteractionType::Purchase, Some("music"), Some("0xcreator1"), vec!["psy-trance"], None),
    ];

    let fold = |events: &[InteractionEvent]| {
        let mut prefs = UserPreferences {
            user_address: "0xreplay".to_string(),
            ..Default::default()
        };
        for event in events {
            apply_interaction_to_prefs(&mut prefs, event, EvictionPolicy::default());
        }
        prefs
    };

    let first = fold(&events);
    let second = fold(&events);

    assert_eq!(first.tag_preferences, second.tag_preferences);
    assert_eq!(first.creator_preferences, second.creator_preferences);
    assert_eq!(first.snap_affinity, second.snap_affinity);
    assert_eq!(first.art_affinity, second.art_affinity);
    assert_eq!(first.music_affinity, second.music_affinity);
    assert_eq!(first.flix_affinity, second.flix_affinity);
    assert_eq!(first.total_likes, second.total_likes);
    assert_eq!(first.total_purchases, second.total_purchases);
    assert_eq!(first.total_views, second.total_views);
}

// -----------------------------------------------------------------------
// BUG-CAP-OFF-BY-ONE regression: apply_interaction_to_prefs must actually
// enforce MAX_TAG_PREFS/MAX_CREATOR_PREFS, not let the map grow to cap + 1
// before eviction ever takes effect. evict() itself is untouched (and its
// own unit tests above still pass unmodified) — the fix was in the target
// size the caller passes it.
// -----------------------------------------------------------------------

#[test]
fn tag_preferences_never_exceeds_max_tag_prefs_once_the_cap_is_crossed() {
    let mut prefs = UserPreferences::default();
    // Fill to exactly the cap, then push one more distinct tag past it.
    for i in 0..=MAX_TAG_PREFS {
        let tag = format!("tag-{i}");
        let event = make_event(InteractionType::View, None, None, vec![&tag], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());
    }
    assert_eq!(
        prefs.tag_preferences.len(),
        MAX_TAG_PREFS,
        "cap must hold exactly at MAX_TAG_PREFS, not drift to MAX_TAG_PREFS + 1"
    );
}

#[test]
fn creator_preferences_never_exceeds_max_creator_prefs_once_the_cap_is_crossed() {
    let mut prefs = UserPreferences::default();
    for i in 0..=MAX_CREATOR_PREFS {
        let creator = format!("0xcreator{i}");
        let event = make_event(InteractionType::View, None, Some(&creator), vec![], None);
        apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default());
    }
    assert_eq!(
        prefs.creator_preferences.len(),
        MAX_CREATOR_PREFS,
        "cap must hold exactly at MAX_CREATOR_PREFS, not drift to MAX_CREATOR_PREFS + 1"
    );
}

// -----------------------------------------------------------------------
// UserPreferences::top_tags / top_creators
// -----------------------------------------------------------------------

#[test]
fn top_tags_excludes_entries_at_or_below_threshold() {
    let mut prefs = UserPreferences::default();
    prefs.tag_preferences.insert("weak".into(), 0.6); // exactly at threshold — excluded (strict >)
    prefs.tag_preferences.insert("strong".into(), 0.61);

    let top = prefs.top_tags(10);
    assert_eq!(top, vec!["strong".to_string()]);
}

#[test]
fn top_tags_uses_the_raised_065_threshold_once_over_20_tags() {
    let mut prefs = UserPreferences::default();
    for i in 0..21 {
        prefs.tag_preferences.insert(format!("tag{i}").into(), 0.62);
    }
    // All 21 are 0.62 — below the raised 0.65 threshold that applies once len() > 20.
    assert!(prefs.top_tags(50).is_empty());
}

#[test]
fn top_tags_sorted_descending_and_truncated_to_limit() {
    let mut prefs = UserPreferences::default();
    prefs.tag_preferences.insert("low".into(), 0.65);
    prefs.tag_preferences.insert("mid".into(), 0.8);
    prefs.tag_preferences.insert("high".into(), 0.95);

    let top = prefs.top_tags(2);
    assert_eq!(top, vec!["high".to_string(), "mid".to_string()]);
}

#[test]
fn top_creators_excludes_entries_at_or_below_0_6() {
    let mut prefs = UserPreferences::default();
    prefs.creator_preferences.insert("0xweak".into(), 0.6);
    prefs.creator_preferences.insert("0xstrong".into(), 0.61);

    let top = prefs.top_creators(10);
    assert_eq!(top, vec!["0xstrong".to_string()]);
}

#[test]
fn top_creators_sorted_descending_and_truncated_to_limit() {
    let mut prefs = UserPreferences::default();
    prefs.creator_preferences.insert("0xa".into(), 0.7);
    prefs.creator_preferences.insert("0xb".into(), 0.9);
    prefs.creator_preferences.insert("0xc".into(), 0.8);

    let top = prefs.top_creators(2);
    assert_eq!(top, vec!["0xb".to_string(), "0xc".to_string()]);
}

#[test]
fn top_tags_and_top_creators_empty_on_fresh_preferences() {
    let prefs = UserPreferences::default();
    assert!(prefs.top_tags(10).is_empty());
    assert!(prefs.top_creators(10).is_empty());
}
