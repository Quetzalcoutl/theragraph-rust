//! NFT Feature Extraction
//!
//! Extracts features from NFT metadata for recommendation matching.
//! Analyzes metadata to identify tags, style, mood, genre, etc.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashSet;
use tracing::info;
use uuid::Uuid;

/// Extracted features from an NFT
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NftFeatures {
    pub nft_id: String,
    pub contract_address: String,
    pub token_id: i64,
    pub tags: Vec<String>,
    pub primary_color: Option<String>,
    pub style: Option<String>,
    pub mood: Option<String>,
    pub genre: Option<String>,
    pub engagement_score: f32,
    pub trending_score: f32,
    pub quality_score: f32,
}

/// Scoring-only projection of `NftFeatures`.
///
/// Passed into the hot scoring path (rayon parallel loop) instead of the full
/// `NftFeatures` so we serialize 4 fields to Redis rather than 11. The identity
/// and metadata fields (`nft_id`, `contract_address`, `primary_color`, etc.) are
/// only needed for persistence and candidate enrichment, not for scoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringFeatures {
    pub tags: Vec<String>,
    pub engagement_score: f32,
    pub trending_score: f32,
    pub quality_score: f32,
}

impl From<&NftFeatures> for ScoringFeatures {
    fn from(f: &NftFeatures) -> Self {
        Self {
            tags: f.tags.clone(),
            engagement_score: f.engagement_score,
            trending_score: f.trending_score,
            quality_score: f.quality_score,
        }
    }
}

/// Database row for features
#[derive(Debug, sqlx::FromRow)]
struct FeaturesRow {
    nft_id: Uuid,
    contract_address: String,
    token_id: i64,
    tags: Option<Vec<String>>,
    primary_color: Option<String>,
    style: Option<String>,
    mood: Option<String>,
    genre: Option<String>,
    engagement_score: f32,
    trending_score: f32,
    quality_score: f32,
}

// Keyword dictionaries for feature extraction
// These will be used when processing new NFT metadata

const ART_STYLES: &[&str] = &[
    "abstract",
    "realistic",
    "surreal",
    "minimalist",
    "maximalist",
    "impressionist",
    "expressionist",
    "pop art",
    "digital",
    "3d",
    "pixel",
    "generative",
    "ai",
    "hand-drawn",
    "photography",
];

const MUSIC_GENRES: &[&str] = &[
    "rock",
    "pop",
    "jazz",
    "classical",
    "electronic",
    "hip-hop",
    "r&b",
    "country",
    "folk",
    "metal",
    "punk",
    "indie",
    "ambient",
    "lo-fi",
    "house",
    "techno",
    "dubstep",
    "reggae",
    "soul",
    "blues",
];

const MOOD_KEYWORDS: &[&str] = &[
    "happy",
    "sad",
    "melancholic",
    "energetic",
    "calm",
    "peaceful",
    "dark",
    "bright",
    "mysterious",
    "playful",
    "romantic",
    "angry",
    "nostalgic",
    "hopeful",
    "dreamy",
    "intense",
    "relaxing",
];

const NATURE_TAGS: &[&str] = &[
    "nature",
    "landscape",
    "ocean",
    "mountain",
    "forest",
    "desert",
    "sky",
    "sunset",
    "sunrise",
    "flowers",
    "animals",
    "wildlife",
    "beach",
    "river",
    "waterfall",
    "trees",
    "garden",
];

const COLOR_KEYWORDS: &[&str] = &[
    "red",
    "blue",
    "green",
    "yellow",
    "purple",
    "orange",
    "pink",
    "black",
    "white",
    "gold",
    "silver",
    "pastel",
    "neon",
    "monochrome",
    "colorful",
    "vibrant",
    "muted",
    "warm",
    "cool",
];

/// Extract features from NFT metadata
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
    let mut genre = None;
    let mut primary_color = None;

    // Extract from name (bucket 2)
    if let Some(name) = metadata.get("name").and_then(|v| v.as_str()) {
        extract_keywords_from_text(
            name,
            &mut keyword_tags,
            &mut style,
            &mut mood,
            &mut genre,
            &mut primary_color,
        );
    }

    // Extract from description (bucket 2)
    if let Some(desc) = metadata.get("description").and_then(|v| v.as_str()) {
        extract_keywords_from_text(
            desc,
            &mut keyword_tags,
            &mut style,
            &mut mood,
            &mut genre,
            &mut primary_color,
        );
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
                    "mood" | "vibe" => mood = Some(value.to_lowercase()),
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
        engagement_score: 0.0, // Updated separately
        trending_score: 0.0,   // Updated separately
        quality_score: creator_quality_score,
    }
}

/// True if `keyword` appears in `text` as a whole word — not a substring of another word.
/// Checks that the character before and after the match (if present) is not an ASCII letter.
/// This prevents "pop" from matching "popular", "rock" from matching "rocket", etc.
/// Non-alpha boundary characters (spaces, punctuation, digits) always count as word boundaries,
/// so "r&b", "lo-fi", "pop art", "3d" all match correctly.
fn matches_whole_word(text: &str, keyword: &str) -> bool {
    let klen = keyword.len();
    if klen == 0 {
        return false;
    }
    let bytes = text.as_bytes();
    let mut pos = 0;
    while let Some(offset) = text[pos..].find(keyword) {
        let abs = pos + offset;
        let before_ok = abs == 0 || !bytes[abs - 1].is_ascii_alphabetic();
        let after_ok = abs + klen >= text.len() || !bytes[abs + klen].is_ascii_alphabetic();
        if before_ok && after_ok {
            return true;
        }
        pos = abs + 1;
    }
    false
}

fn extract_keywords_from_text(
    text: &str,
    tags: &mut HashSet<String>,
    style: &mut Option<String>,
    mood: &mut Option<String>,
    genre: &mut Option<String>,
    color: &mut Option<String>,
) {
    let lower = text.to_lowercase();

    for &s in ART_STYLES {
        if matches_whole_word(&lower, s) {
            tags.insert(s.to_string());
            if style.is_none() {
                *style = Some(s.to_string());
            }
        }
    }

    for &g in MUSIC_GENRES {
        if matches_whole_word(&lower, g) {
            tags.insert(g.to_string());
            if genre.is_none() {
                *genre = Some(g.to_string());
            }
        }
    }

    for &m in MOOD_KEYWORDS {
        if matches_whole_word(&lower, m) {
            tags.insert(m.to_string());
            if mood.is_none() {
                *mood = Some(m.to_string());
            }
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
            if color.is_none() {
                *color = Some(c.to_string());
            }
        }
    }
}

/// Save extracted features to database
pub async fn save_features(pool: &PgPool, features: &NftFeatures) -> Result<()> {
    let nft_uuid = Uuid::parse_str(&features.nft_id)
        .map_err(|_| anyhow::anyhow!("Invalid UUID in nft_id: {}", features.nft_id))?;

    sqlx::query(
        r#"
        INSERT INTO nft_features 
            (id, nft_id, contract_address, token_id, tags, primary_color, 
             style, mood, genre, engagement_score, trending_score, quality_score,
             inserted_at, updated_at)
        VALUES 
            (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW(), NOW())
        ON CONFLICT (nft_id) DO UPDATE SET
            tags = $4,
            primary_color = $5,
            style = $6,
            mood = $7,
            genre = $8,
            engagement_score = $9,
            trending_score = $10,
            quality_score = $11,
            updated_at = NOW()
        "#,
    )
    .bind(nft_uuid)
    .bind(&features.contract_address)
    .bind(features.token_id)
    .bind(&features.tags)
    .bind(&features.primary_color)
    .bind(&features.style)
    .bind(&features.mood)
    .bind(&features.genre)
    .bind(features.engagement_score)
    .bind(features.trending_score)
    .bind(features.quality_score)
    .execute(pool)
    .await?;

    Ok(())
}

/// Convert a raw DB row into the public `NftFeatures` model.
fn features_row_to_model(row: FeaturesRow) -> NftFeatures {
    NftFeatures {
        nft_id: row.nft_id.to_string(),
        contract_address: row.contract_address,
        token_id: row.token_id,
        tags: row.tags.unwrap_or_default(),
        primary_color: row.primary_color,
        style: row.style,
        mood: row.mood,
        genre: row.genre,
        engagement_score: row.engagement_score,
        trending_score: row.trending_score,
        quality_score: row.quality_score,
    }
}

/// Batch-fetch NftFeatures for many NFTs in a single SQL query — eliminates N+1.
///
/// Returns only entries that exist in the `nft_features` table; missing IDs are
/// simply absent from the output. Callers should treat absence as `None`.
pub async fn get_features_batch(pool: &PgPool, nft_ids: &[Uuid]) -> Result<Vec<NftFeatures>> {
    if nft_ids.is_empty() {
        return Ok(vec![]);
    }

    let rows: Vec<FeaturesRow> = sqlx::query_as::<_, FeaturesRow>(
        r#"
        SELECT nft_id, contract_address, token_id, tags, primary_color,
               style, mood, genre,
               engagement_score::real, trending_score::real, quality_score::real
        FROM nft_features
        WHERE nft_id = ANY($1)
        "#,
    )
    .bind(nft_ids)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(features_row_to_model).collect())
}

/// Update engagement scores for all NFTs (run periodically)
pub async fn update_engagement_scores(pool: &PgPool) -> Result<u64> {
    // Log-compressed linear formula: LN(1 + raw) / 5.0, clamped to [0, 1].
    //
    // The previous sigmoid σ(0.1*raw) evaluated to 0.5 for zero-engagement items
    // (σ(0) = 0.5 by definition), compressing the useful signal range into [0.5, 1.0]
    // and giving every new NFT a phantom 0.5 engagement score.
    //
    // This formula returns 0 for (likes=0, buys=0, comments=0) and reaches 1.0 at
    // roughly 150 weighted interactions — a sensible saturation point for an early
    // platform. The /5.0 divisor = LN(1 + 148) ≈ 5.0.
    //
    // Weighted raw: likes×1 + buys×3 + comments×0.5
    let result = sqlx::query(
        r#"
        UPDATE nft_features f SET
            engagement_score = LEAST(1.0::double precision,
                LN(1.0 + (
                    COALESCE(n.likes_count, 0)::double precision +
                    COALESCE(n.buys_count, 0)::double precision * 3.0 +
                    COALESCE(n.comments_count, 0)::double precision * 0.5
                )) / 5.0
            ),
            updated_at = NOW()
        FROM nfts n
        WHERE f.nft_id = n.id
        "#,
    )
    .execute(pool)
    .await?;

    info!(
        "📊 Updated engagement scores for {} NFTs",
        result.rows_affected()
    );
    Ok(result.rows_affected())
}

/// Update trending scores based on recent activity (run hourly)
pub async fn update_trending_scores(pool: &PgPool) -> Result<u64> {
    // TAG-S27-08: Rewritten as CTE + LEFT JOIN to eliminate the correlated subquery.
    // The old form ran one SELECT on user_interactions PER ROW in nft_features, holding
    // a write lock on the entire nft_features table for the duration — minutes on large
    // datasets. The CTE form does a single GROUP BY over user_interactions once, then
    // joins the result set. A covering index on user_interactions(nft_id, created_at)
    // (migration 007) removes the sequential scan inside that GROUP BY.
    //
    // The LEFT JOIN on nft_features AS base handles rows that have no recent interactions:
    // s.raw_score is NULL → COALESCE → 0.0 → trending_score = 0.
    let result = sqlx::query(
        r#"
        WITH recent_scores AS (
            SELECT
                nft_id,
                SUM(
                    CASE interaction_type
                        WHEN 'like'     THEN 1.0
                        WHEN 'purchase' THEN 3.0
                        WHEN 'view'     THEN 0.1
                        ELSE 0.5
                    END * EXP(-EXTRACT(EPOCH FROM (NOW() - created_at)) / 86400.0)
                ) AS raw_score
            FROM user_interactions
            WHERE created_at > NOW() - INTERVAL '7 days'
            GROUP BY nft_id
        )
        UPDATE nft_features f
        SET trending_score = LEAST(1.0::double precision,
                LN(1.0 + COALESCE(s.raw_score, 0.0)) / LN(301.0)
            ),
            updated_at = NOW()
        FROM nft_features AS base
        LEFT JOIN recent_scores s ON s.nft_id = base.nft_id
        WHERE f.nft_id = base.nft_id
        "#,
    )
    .execute(pool)
    .await?;

    info!(
        "📈 Updated trending scores for {} NFTs",
        result.rows_affected()
    );
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── helper ─────────────────────────────────────────────────────────────────

    fn run(metadata: serde_json::Value) -> NftFeatures {
        extract_features(
            "00000000-0000-0000-0000-000000000001",
            "0xdeadbeef",
            1,
            "art",
            &metadata,
            0.0,
        )
    }

    // 1. Output fields match input IDs
    #[test]
    fn output_ids_match_input() {
        let features = extract_features(
            "00000000-0000-0000-0000-000000000042",
            "0xAbCdEf",
            7,
            "snap",
            &json!({}),
            0.0,
        );

        assert_eq!(features.nft_id, "00000000-0000-0000-0000-000000000042");
        assert_eq!(features.token_id, 7);
        // contract_address is lowercased by the function
        assert_eq!(features.contract_address, "0xabcdef");
    }

    // 2. quality_score propagated from creator_quality_score
    #[test]
    fn quality_score_from_creator() {
        let score = 0.75_f32;
        let features = extract_features(
            "00000000-0000-0000-0000-000000000002",
            "0xdeadbeef",
            1,
            "art",
            &json!({}),
            score,
        );

        assert!(
            (features.quality_score - score).abs() < f32::EPSILON,
            "quality_score should equal creator_quality_score ({score}); got {}",
            features.quality_score
        );
    }

    // 3. Keyword in name → style extracted
    #[test]
    fn style_extracted_from_name() {
        // "abstract" is the first entry in ART_STYLES
        let features = run(json!({ "name": "An abstract composition" }));

        assert_eq!(
            features.style,
            Some("abstract".to_string()),
            "style should be 'abstract' extracted from name"
        );
        assert!(
            features.tags.contains(&"abstract".to_string()),
            "tags should contain 'abstract'; got {:?}", features.tags
        );
    }

    // 4. Genre keyword in description → genre extracted
    #[test]
    fn genre_extracted_from_description() {
        // "jazz" is in MUSIC_GENRES
        let features = run(json!({ "description": "A smooth jazz inspired piece" }));

        assert_eq!(
            features.genre,
            Some("jazz".to_string()),
            "genre should be 'jazz' extracted from description"
        );
        assert!(
            features.tags.contains(&"jazz".to_string()),
            "tags should contain 'jazz'; got {:?}", features.tags
        );
    }

    // 5. Mood keyword in metadata → mood extracted
    #[test]
    fn mood_extracted() {
        // "peaceful" is in MOOD_KEYWORDS; put it in name to keep things simple
        let features = run(json!({ "name": "A peaceful mountain scene" }));

        assert_eq!(
            features.mood,
            Some("peaceful".to_string()),
            "mood should be 'peaceful' extracted from name"
        );
        assert!(
            features.tags.contains(&"peaceful".to_string()),
            "tags should contain 'peaceful'; got {:?}", features.tags
        );
    }

    // 6. Color keyword → primary_color extracted
    #[test]
    fn color_extracted() {
        // "blue" is the second entry in COLOR_KEYWORDS (after "red").
        // Use a description whose lowercased text contains only "blue" from
        // COLOR_KEYWORDS so the first color hit is deterministic.
        let features = run(json!({ "description": "A vivid blue sky painting" }));

        assert_eq!(
            features.primary_color,
            Some("blue".to_string()),
            "primary_color should be 'blue' extracted from description"
        );
        assert!(
            features.tags.contains(&"blue".to_string()),
            "tags should contain 'blue'; got {:?}", features.tags
        );
    }

    // 7. OpenSea attributes → tags include attribute values
    #[test]
    fn attributes_extracted_as_tags() {
        let features = run(json!({
            "attributes": [
                { "trait_type": "Rarity",  "value": "Legendary" },
                { "trait_type": "Element", "value": "Fire" }
            ]
        }));

        assert!(
            features.tags.contains(&"legendary".to_string()),
            "tags should contain attribute value 'legendary'; got {:?}", features.tags
        );
        assert!(
            features.tags.contains(&"fire".to_string()),
            "tags should contain attribute value 'fire'; got {:?}", features.tags
        );
    }

    // 8. Empty metadata → no style/genre/mood/color, empty tags (except contract_type)
    #[test]
    fn empty_metadata_no_features() {
        let features = run(json!({}));

        assert!(features.style.is_none(),         "style should be None for empty metadata");
        assert!(features.genre.is_none(),          "genre should be None for empty metadata");
        assert!(features.mood.is_none(),           "mood should be None for empty metadata");
        assert!(features.primary_color.is_none(),  "primary_color should be None for empty metadata");
        // Only the contract-type tag ("art") should be present
        assert_eq!(features.tags, vec!["art".to_string()], "only contract-type tag expected; got {:?}", features.tags);
        assert_eq!(features.engagement_score, 0.0);
        assert_eq!(features.trending_score, 0.0);
    }

    // 9. Tags are lowercase (even if metadata has mixed case)
    #[test]
    fn tags_are_lowercase() {
        let features = run(json!({
            "tags": ["Nature", "OCEAN", "Sunset"]
        }));

        for tag in &features.tags {
            assert_eq!(
                *tag,
                tag.to_lowercase(),
                "tag '{tag}' should be lowercase"
            );
        }
        assert!(features.tags.contains(&"nature".to_string()), "tags should contain 'nature'");
        assert!(features.tags.contains(&"ocean".to_string()),  "tags should contain 'ocean'");
        assert!(features.tags.contains(&"sunset".to_string()), "tags should contain 'sunset'");
    }

    // 10. Style attribute trait_type "Style" → sets style field
    #[test]
    fn attribute_style_trait() {
        let features = run(json!({
            "attributes": [
                { "trait_type": "Style", "value": "Impressionist" }
            ]
        }));

        assert_eq!(
            features.style,
            Some("impressionist".to_string()),
            "style should be set from trait_type 'Style' attribute value (lowercased)"
        );
        assert!(
            features.tags.contains(&"impressionist".to_string()),
            "attribute value should also appear in tags; got {:?}", features.tags
        );
    }
}

