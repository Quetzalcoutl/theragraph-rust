//! Global tag string interner.
//!
//! NFT tags are drawn from a mostly-small vocabulary (art styles, music
//! genres, moods, nature/color keywords — see `feature_extractor`'s keyword
//! dictionaries) plus open-vocabulary user-supplied hashtags. A single feed
//! request scores up to 500 NFTs; popular tags like "art" or "abstract"
//! appear on hundreds of them. Before interning, each occurrence held its
//! own `Box<str>` — same 3-8 bytes of content, copied and allocated once per
//! NFT that carries the tag. Interning stores the byte content exactly once
//! per unique tag string and hands out cheap `Arc<str>` clones (a pointer +
//! refcount bump, no allocation) everywhere else.
//!
//! Bounded and evicting (not a plain growing `HashMap`) because hashtags are
//! genuinely open-vocabulary — an unbounded interner is a slow memory leak
//! under adversarial or just high-cardinality tag input. Uses the same
//! bounded-`RwLock<HashMap>`-with-hard-cap shape as
//! `thera-bundler-rust`'s nonce replay cache, not `moka` (already a crate
//! dependency, used by `StampedeCoalescer`) — `moka`'s cache here would need
//! the `future` (async) variant, and interning happens in a synchronous
//! `From` impl on the feature-loading path; reaching for an async cache would
//! force that call site to become async for no benefit an evicting sync map
//! doesn't already provide.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Hard cap on distinct interned tags. Generous relative to any realistic
/// tag vocabulary (keyword dictionaries are a few hundred entries; even
/// heavy hashtag usage across the whole platform is unlikely to sustain
/// more than a few thousand *distinct* live tags at once) — this exists as
/// a backstop against unbounded growth, not a expected-to-hit ceiling.
const MAX_INTERNED_TAGS: usize = 50_000;

fn interner() -> &'static RwLock<HashMap<Box<str>, Arc<str>>> {
    static INTERNER: std::sync::OnceLock<RwLock<HashMap<Box<str>, Arc<str>>>> = std::sync::OnceLock::new();
    INTERNER.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Intern a tag string, returning a shared `Arc<str>`.
///
/// Fast path: read lock, existing entry, clone the `Arc` (refcount bump, no
/// allocation). Slow path (first time this exact tag is seen): write lock,
/// one allocation, cached for every future call with this tag.
///
/// At capacity, clears the whole table rather than evicting piecemeal (same
/// hard-ceiling-then-clear pattern as the bundler's nonce cache) — simple,
/// and correctness-preserving either way since callers only ever need *a*
/// valid `Arc<str>` for the tag, not a *stable* one shared with earlier calls.
pub fn intern_tag(tag: &str) -> Arc<str> {
    if let Some(existing) = interner().read().unwrap_or_else(|e| e.into_inner()).get(tag) {
        return existing.clone();
    }

    let mut map = interner().write().unwrap_or_else(|e| e.into_inner());
    // Re-check under the write lock — another thread may have interned this
    // exact tag between our read-lock miss and acquiring the write lock.
    if let Some(existing) = map.get(tag) {
        return existing.clone();
    }
    if map.len() >= MAX_INTERNED_TAGS {
        map.clear();
    }
    let arc: Arc<str> = Arc::from(tag);
    map.insert(tag.into(), arc.clone());
    arc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_same_content_to_equal_but_independent_arcs() {
        let a = intern_tag("abstract");
        let b = intern_tag("abstract");
        assert_eq!(a, b);
    }

    #[test]
    fn interning_the_same_tag_twice_shares_the_allocation() {
        let a = intern_tag("generative-unique-test-tag");
        let b = intern_tag("generative-unique-test-tag");
        // Arc::ptr_eq proves the second call returned a clone of the same
        // allocation rather than a fresh one — the actual point of interning.
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn different_tags_intern_to_different_arcs() {
        let a = intern_tag("rock");
        let b = intern_tag("jazz");
        assert_ne!(a, b);
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn empty_tag_does_not_panic() {
        let a = intern_tag("");
        assert_eq!(a.as_ref(), "");
    }
}
