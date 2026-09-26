//! Pure NFT feature extraction — no I/O, no async, no DB dependency.
//!
//! `extract_features` is the only public entry point. All keyword dictionaries
//! and text-matching helpers live here so tuning a keyword list or adding a
//! new content type never touches the DB layer (`feature_store.rs`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

use super::tag_interner::intern_tag;

/// Extracted features from an NFT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NftFeatures {
    pub nft_id: String,
    pub contract_address: String,
    pub token_id: i64,
    pub tags: Vec<String>,
    pub primary_color: Option<String>,
    pub style: Option<String>,
    pub mood: Option<String>,
    /// Set only from an explicit creator-typed metadata attribute
    /// (`trait_type` of `"genre"`/`"music genre"`) — never guessed from free
    /// text. Confirmed unused by scoring (`candidate_repository::load_features`
    /// never copies it forward) and kept only as descriptive Postgres metadata.
    ///
    /// The authoritative genre signal for scoring/discovery lives in `tags`:
    /// real genre values from the Elixir `nfts.genre`/`genres[]` columns are
    /// folded into `tags` (as kebab-case slugs) at ingestion time — see
    /// `event_processor::elixir_db::process_enrichment`. A prior naive
    /// 20-keyword substring guesser (`MUSIC_GENRES`) that scanned NFT
    /// name/description text to populate this field was removed as dead
    /// weight: its only production caller never passed name/description into
    /// `extract_features` in the first place, so it never actually fired.
    pub genre: Option<String>,
    pub engagement_score: f32,
    pub trending_score: f32,
    pub quality_score: f32,
}

/// Scoring-only projection of `NftFeatures`.
///
/// Passed into the hot scoring path (rayon parallel loop) instead of the full
/// `NftFeatures` so we serialize 4 fields to Redis rather than 11. Identity
/// and metadata fields are only needed for persistence and candidate enrichment.
///
/// Tags use `Arc<str>` (16 bytes) instead of `String` (24 bytes) — tags are
/// immutable after extraction, so the capacity field is wasted. At avg 8 tags
/// × 500 scored NFTs per feed this saves ~32KB of heap metadata per feed cycle.
/// `Arc` (not `Box`) because tags are drawn from a mostly-small, highly
/// repeated vocabulary (art styles, music genres, moods — see the keyword
/// dictionaries below — plus open-vocabulary hashtags): `intern_tag` shares
/// one allocation across every NFT carrying the same tag rather than each
/// holding its own copy of the same 3-8 bytes. See `tag_interner` module docs.
///
/// The outer container is `Box<[Arc<str>]>` rather than `Vec<Arc<str>>` for the
/// same reason (Cloudflare 1.1.1.1 DNS cache pattern): this list is never
/// pushed/extended after `From<&NftFeatures>` builds it, so the Vec capacity
/// field (8 bytes) is dead weight — Box<[T]> allocates exactly len, no slack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringFeatures {
    pub tags: Box<[Arc<str>]>,
    pub engagement_score: f32,
    pub trending_score: f32,
    pub quality_score: f32,
}

impl From<&NftFeatures> for ScoringFeatures {
    fn from(f: &NftFeatures) -> Self {
        Self {
            tags: f.tags.iter().map(|s| intern_tag(s.as_str())).collect(),
            engagement_score: f.engagement_score,
            trending_score: f.trending_score,
            quality_score: f.quality_score,
        }
    }
}

// ── Keyword dictionaries ──────────────────────────────────────────────────────

const ART_STYLES: &[&str] = &[
    "abstract", "realistic", "surreal", "minimalist", "maximalist",
    "impressionist", "expressionist", "pop art", "digital", "3d",
    "pixel", "generative", "ai", "hand-drawn", "photography",
];

const MOOD_KEYWORDS: &[&str] = &[
    "happy", "sad", "melancholic", "energetic", "calm", "peaceful", "dark",
    "bright", "mysterious", "playful", "romantic", "angry", "nostalgic",
    "hopeful", "dreamy", "intense", "relaxing",
];

const NATURE_TAGS: &[&str] = &[
    "nature", "landscape", "ocean", "mountain", "forest", "desert", "sky",
    "sunset", "sunrise", "flowers", "animals", "wildlife", "beach", "river",
    "waterfall", "trees", "garden",
];

const COLOR_KEYWORDS: &[&str] = &[
    "red", "blue", "green", "yellow", "purple", "orange", "pink", "black",
    "white", "gold", "silver", "pastel", "neon", "monochrome", "colorful",
    "vibrant", "muted", "warm", "cool",
];

// ── Public entry point ────────────────────────────────────────────────────────

/// Extract features from NFT metadata — pure, synchronous, no I/O.
pub fn extract_features(
    nft_id: &str,
    contract_address: &str,
    token_id: i64,
    contract_type: &str,
    metadata: &Value,
    creator_quality_score: f32,
) -> NftFeatures {
    // TAG-S27-10: Three-bucket tag collection so truncation preserves user-provided
    // hashtags over auto-extracted keywords. The old single-HashSet + alphabetical sort
    // dropped hashtags with w/x/y/z prefixes ("zen", "xr", "yearning") in favour of
    // alphabetically-early but lower-signal keywords like "abstract" or "blue".
    //
    // Bucket 0 (highest priority): user-provided hashtags from all hashtag fields.
    // Bucket 1: OpenSea-style attribute values (explicitly typed by creator).
    // Bucket 2 (lowest priority): auto-extracted keywords from name/description,
    //          legacy tags field, and contract_type.
    let mut hashtag_tags: HashSet<String> = HashSet::new();
    let mut attribute_tags: HashSet<String> = HashSet::new();
    let mut keyword_tags: HashSet<String> = HashSet::new();

    let mut style = None;
    let mut mood = None;
    // Populated only from an explicit "genre"/"music genre" attribute below —
    // never guessed from name/description text. See the doc comment on
    // `NftFeatures::genre`.
    let mut genre = None;
    let mut primary_color = None;

    // Extract from name (bucket 2)
    if let Some(name) = metadata.get("name").and_then(|v| v.as_str()) {
        extract_keywords_from_text(name, &mut keyword_tags, &mut style, &mut mood, &mut primary_color);
    }

    // Extract from description (bucket 2)
    if let Some(desc) = metadata.get("description").and_then(|v| v.as_str()) {
        extract_keywords_from_text(desc, &mut keyword_tags, &mut style, &mut mood, &mut primary_color);
    }

    // Extract from attributes (OpenSea style) → bucket 1
    if let Some(attributes) = metadata.get("attributes").and_then(|v| v.as_array()) {
        for attr in attributes {
            if let (Some(trait_type), Some(value)) = (
                attr.get("trait_type").and_then(|v| v.as_str()),
                attr.get("value").and_then(|v| v.as_str()),
            ) {
                attribute_tags.insert(value.to_lowercase());
                match trait_type.to_lowercase().as_str() {
                    "style" | "art style" => style = Some(value.to_lowercase()),
                    "mood" | "vibe"       => mood  = Some(value.to_lowercase()),
                    "genre" | "music genre" => genre = Some(value.to_lowercase()),
                    "color" | "primary color" => primary_color = Some(value.to_lowercase()),
                    _ => {}
                }
            }
        }
    }

    // User-provided hashtags from top-level metadata → bucket 0 (highest priority)
    if let Some(hashtag_array) = metadata.get("hashtags").and_then(|v| v.as_array()) {
        for hashtag in hashtag_array.iter().take(3) {
            if let Some(h) = hashtag.as_str() {
                let clean_tag = h.trim().to_lowercase();
                if !clean_tag.is_empty() {
                    hashtag_tags.insert(clean_tag);
                }
            }
        }
    }

    // Hashtags from special_event → bucket 0
    if let Some(event) = metadata.get("special_event") {
        if let Some(event_hashtags) = event.get("hashtags").and_then(|v| v.as_array()) {
            for hashtag in event_hashtags.iter().take(3) {
                if let Some(h) = hashtag.as_str() {
                    let clean_tag = h.trim().to_lowercase();
                    if !clean_tag.is_empty() {
                        hashtag_tags.insert(clean_tag);
                    }
                }
            }
        }
    }

    // Hashtags from content-specific metadata → bucket 0
    for metadata_key in &["therasnap_metadata", "theraart_metadata", "theramusic_metadata", "theraflix_metadata"] {
        if let Some(content_meta) = metadata.get(*metadata_key) {
            if let Some(content_hashtags) = content_meta.get("hashtags").and_then(|v| v.as_array()) {
                for hashtag in content_hashtags.iter().take(3) {
                    if let Some(h) = hashtag.as_str() {
                        let clean_tag = h.trim().to_lowercase();
                        if !clean_tag.is_empty() {
                            hashtag_tags.insert(clean_tag);
                        }
                    }
                }
            }
        }
    }

    // Legacy tags field → bucket 2
    if let Some(tag_array) = metadata.get("tags").and_then(|v| v.as_array()) {
        for tag in tag_array {
            if let Some(t) = tag.as_str() {
                keyword_tags.insert(t.to_lowercase());
            }
        }
    }

    // Contract type → bucket 2
    keyword_tags.insert(contract_type.to_lowercase());

    // Merge buckets in priority order (0 → 1 → 2); sort alphabetically within each
    // bucket for determinism; deduplicate across buckets; truncate to MAX_NFT_TAGS.
    const MAX_NFT_TAGS: usize = 10;
    let mut tags_vec: Vec<String> = Vec::with_capacity(MAX_NFT_TAGS + 4);
    let mut seen: HashSet<String> = HashSet::new();

    let mut emit_bucket = |bucket: HashSet<String>| {
        let mut sorted: Vec<String> = bucket.into_iter().collect();
        sorted.sort();
        for t in sorted {
            if seen.insert(t.clone()) {
                tags_vec.push(t);
            }
        }
    };
    emit_bucket(hashtag_tags);
    emit_bucket(attribute_tags);
    emit_bucket(keyword_tags);

    tags_vec.truncate(MAX_NFT_TAGS);

    NftFeatures {
        nft_id: nft_id.to_string(),
        contract_address: contract_address.to_lowercase(),
        token_id,
        tags: tags_vec,
        primary_color,
        style,
        mood,
        genre,
        engagement_score: 0.0,
        trending_score: 0.0,
        quality_score: creator_quality_score,
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// True if `keyword` appears in `text` as a whole word — not a substring of another word.
/// Non-alpha boundary characters (spaces, punctuation, digits) always count as word boundaries,
/// so "r&b", "lo-fi", "pop art", "3d" all match correctly.
fn matches_whole_word(text: &str, keyword: &str) -> bool {
    let klen = keyword.len();
    if klen == 0 { return false; }
    let bytes = text.as_bytes();
    let mut pos = 0;
    while let Some(offset) = text[pos..].find(keyword) {
        let abs = pos + offset;
        let before_ok = abs == 0 || !bytes[abs - 1].is_ascii_alphabetic();
        let after_ok = abs + klen >= text.len() || !bytes[abs + klen].is_ascii_alphabetic();
        if before_ok && after_ok { return true; }
        pos = abs + 1;
    }
    false
}

fn extract_keywords_from_text(
    text: &str,
    tags: &mut HashSet<String>,
    style: &mut Option<String>,
    mood: &mut Option<String>,
    color: &mut Option<String>,
) {
    let lower = text.to_lowercase();

    for &s in ART_STYLES {
        if matches_whole_word(&lower, s) {
            tags.insert(s.to_string());
            if style.is_none() { *style = Some(s.to_string()); }
        }
    }
    for &m in MOOD_KEYWORDS {
        if matches_whole_word(&lower, m) {
            tags.insert(m.to_string());
            if mood.is_none() { *mood = Some(m.to_string()); }
        }
    }
    for &n in NATURE_TAGS {
        if matches_whole_word(&lower, n) {
            tags.insert(n.to_string());
        }
    }
    for &c in COLOR_KEYWORDS {
        if matches_whole_word(&lower, c) {
            tags.insert(c.to_string());
            if color.is_none() { *color = Some(c.to_string()); }
        }
    }
}

// ── Tests (pure — no DB, no async, no PgPool) ─────────────────────────────────


#[cfg(test)]
mod tests;
