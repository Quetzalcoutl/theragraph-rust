use super::*;
use crate::recommendation::cache::SessionSignal;

fn make_signal(tags: Vec<&str>, creator: Option<&str>, weight: f32, age_secs: i64) -> SessionSignal {
    SessionSignal {
        tags: tags.into_iter().map(|s| s.into()).collect(),
        creator: creator.map(|s| s.into()),
        interaction_weight: weight,
        ts_unix: 1_000_000 - age_secs,
    }
}

#[test]
fn fresh_signal_full_weight() {
    let signal = make_signal(vec!["art"], Some("0xcreator"), 1.0, 0);
    let (tb, cb) = compute_session_boost_maps(&[signal], 1_000_000);
    let art = tb["art"];
    let creator = cb["0xcreator"];
    assert!((art - 1.0).abs() < 1e-5, "fresh signal should have full tag weight, got {art}");
    assert!(
        (creator - 1.0).abs() < 1e-5,
        "fresh signal should have full creator weight, got {creator}"
    );
}

#[test]
fn signal_at_1800s_exp_decay() {
    // 30-min half-life: exp(-1800/1800) = exp(-1.0) ≈ 0.3679
    let signal = make_signal(vec!["music"], None, 1.0, 1800);
    let (tb, _) = compute_session_boost_maps(&[signal], 1_000_000);
    let music = tb["music"];
    let expected = (-1800_f32 / 1800.0).exp();
    assert!(
        (music - expected).abs() < 1e-4,
        "30-min-old signal should decay to {expected:.4}, got {music}"
    );
}

#[test]
fn old_signal_near_zero() {
    // 4 hours old → exp(-4) ≈ 0.0183
    let signal = make_signal(vec!["snap"], None, 1.0, 14_400);
    let (tb, _) = compute_session_boost_maps(&[signal], 1_000_000);
    let snap = tb["snap"];
    assert!(snap < 0.02, "4-hour-old signal should be near zero, got {snap}");
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
