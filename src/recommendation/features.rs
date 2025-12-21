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

#[allow(dead_code)]
const ART_STYLES: &[&str] = &[
    "abstract", "realistic", "surreal", "minimalist", "maximalist",
    "impressionist", "expressionist", "pop art", "digital", "3d",
    "pixel", "generative", "ai", "hand-drawn", "photography",
];

#[allow(dead_code)]
const MUSIC_GENRES: &[&str] = &[
    "rock", "pop", "jazz", "classical", "electronic", "hip-hop",
    "r&b", "country", "folk", "metal", "punk", "indie", "ambient",
    "lo-fi", "house", "techno", "dubstep", "reggae", "soul", "blues",
];

#[allow(dead_code)]
const MOOD_KEYWORDS: &[&str] = &[
    "happy", "sad", "melancholic", "energetic", "calm", "peaceful",
    "dark", "bright", "mysterious", "playful", "romantic", "angry",
    "nostalgic", "hopeful", "dreamy", "intense", "relaxing",
];

#[allow(dead_code)]
const NATURE_TAGS: &[&str] = &[
    "nature", "landscape", "ocean", "mountain", "forest", "desert",
    "sky", "sunset", "sunrise", "flowers", "animals", "wildlife",
    "beach", "river", "waterfall", "trees", "garden",
];

#[allow(dead_code)]
const COLOR_KEYWORDS: &[&str] = &[
    "red", "blue", "green", "yellow", "purple", "orange", "pink",
    "black", "white", "gold", "silver", "pastel", "neon", "monochrome",
    "colorful", "vibrant", "muted", "warm", "cool",
];

/// Extract features from NFT metadata
#[allow(dead_code)]
pub fn extract_features(
    nft_id: &str,
    contract_address: &str,
    token_id: i64,
    contract_type: &str,
    metadata: &Value,
    creator_quality_score: f32,
) -> NftFeatures {
    let mut tags = HashSet::new();
    let mut style = None;
    let mut mood = None;
    let mut genre = None;
    let mut primary_color = None;
    
    // Extract from name
    if let Some(name) = metadata.get("name").and_then(|v| v.as_str()) {
        extract_keywords_from_text(name, &mut tags, &mut style, &mut mood, &mut genre, &mut primary_color);
    }
    
    // Extract from description
    if let Some(desc) = metadata.get("description").and_then(|v| v.as_str()) {
        extract_keywords_from_text(desc, &mut tags, &mut style, &mut mood, &mut genre, &mut primary_color);
    }
    
    // Extract from attributes (OpenSea style)
    if let Some(attributes) = metadata.get("attributes").and_then(|v| v.as_array()) {
        for attr in attributes {
            if let (Some(trait_type), Some(value)) = (
                attr.get("trait_type").and_then(|v| v.as_str()),
                attr.get("value").and_then(|v| v.as_str())
            ) {
                tags.insert(value.to_lowercase());
                
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
    
    // Extract from explicit tags field
    if let Some(tag_array) = metadata.get("tags").and_then(|v| v.as_array()) {
        for tag in tag_array {
            if let Some(t) = tag.as_str() {
                tags.insert(t.to_lowercase());
            }
        }
    }
    
    // Add contract type as a tag
    tags.insert(contract_type.to_lowercase());
    
    // Convert to sorted vec for consistency
    let mut tags_vec: Vec<String> = tags.into_iter().collect();
    tags_vec.sort();
    
    NftFeatures {
        nft_id: nft_id.to_string(),
        contract_address: contract_address.to_lowercase(),
        token_id,
        tags: tags_vec,
        primary_color,
        style,
        mood,
        genre,
        engagement_score: 0.0,  // Updated separately
        trending_score: 0.0,    // Updated separately
        quality_score: creator_quality_score,
    }
}

#[allow(dead_code)]
fn extract_keywords_from_text(
    text: &str,
    tags: &mut HashSet<String>,
    style: &mut Option<String>,
    mood: &mut Option<String>,
    genre: &mut Option<String>,
    color: &mut Option<String>,
) {
    let lower = text.to_lowercase();
    
    // Check for art styles
    for &s in ART_STYLES {
        if lower.contains(s) {
            tags.insert(s.to_string());
            if style.is_none() {
                *style = Some(s.to_string());
            }
        }
    }
    
    // Check for music genres
    for &g in MUSIC_GENRES {
        if lower.contains(g) {
            tags.insert(g.to_string());
            if genre.is_none() {
                *genre = Some(g.to_string());
            }
        }
    }
    
    // Check for moods
    for &m in MOOD_KEYWORDS {
        if lower.contains(m) {
            tags.insert(m.to_string());
            if mood.is_none() {
                *mood = Some(m.to_string());
            }
        }
    }
    
    // Check for nature tags
    for &n in NATURE_TAGS {
        if lower.contains(n) {
            tags.insert(n.to_string());
        }
    }
    
    // Check for colors
    for &c in COLOR_KEYWORDS {
        if lower.contains(c) {
            tags.insert(c.to_string());
            if color.is_none() {
                *color = Some(c.to_string());
            }
        }
    }
}

/// Save extracted features to database
#[allow(dead_code)]
pub async fn save_features(pool: &PgPool, features: &NftFeatures) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO nft_features 
            (id, nft_id, contract_address, token_id, tags, primary_color, 
             style, mood, genre, engagement_score, trending_score, quality_score,
             inserted_at, updated_at)
        VALUES 
            (gen_random_uuid(), $1::uuid, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW(), NOW())
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
        "#
    )
    .bind(&features.nft_id)
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

/// Get features for an NFT
pub async fn get_features(pool: &PgPool, nft_id: &str) -> Result<Option<NftFeatures>> {
    let result = sqlx::query_as::<_, FeaturesRow>(
        r#"
        SELECT nft_id, contract_address, token_id, tags, primary_color,
               style, mood, genre, engagement_score, trending_score, quality_score
        FROM nft_features
        WHERE nft_id = $1::uuid
        "#
    )
    .bind(nft_id)
    .fetch_optional(pool)
    .await?;
    
    Ok(result.map(|row| NftFeatures {
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
    }))
}

/// Update engagement scores for all NFTs (run periodically)
pub async fn update_engagement_scores(pool: &PgPool) -> Result<u64> {
    // Calculate engagement score based on likes, buys, comments
    // Normalized to 0-1 range using sigmoid
    let result = sqlx::query(
        r#"
        UPDATE nft_features f SET
            engagement_score = 1.0 / (1.0 + EXP(-0.1 * (
                COALESCE(n.likes_count, 0) + 
                COALESCE(n.buys_count, 0) * 3 + 
                COALESCE(n.comments_count, 0) * 0.5
            ))),
            updated_at = NOW()
        FROM nfts n
        WHERE f.nft_id = n.id
        "#
    )
    .execute(pool)
    .await?;
    
    info!("📊 Updated engagement scores for {} NFTs", result.rows_affected());
    Ok(result.rows_affected())
}

/// Update trending scores based on recent activity (run hourly)
pub async fn update_trending_scores(pool: &PgPool) -> Result<u64> {
    // Trending = recent engagement with time decay
    let result = sqlx::query(
        r#"
        UPDATE nft_features f SET
            trending_score = COALESCE((
                SELECT SUM(
                    CASE interaction_type 
                        WHEN 'like' THEN 1.0
                        WHEN 'purchase' THEN 3.0
                        WHEN 'view' THEN 0.1
                        ELSE 0.5
                    END * EXP(-EXTRACT(EPOCH FROM (NOW() - created_at)) / 86400.0)
                )
                FROM user_interactions i
                WHERE i.nft_id = f.nft_id
                AND i.created_at > NOW() - INTERVAL '7 days'
            ), 0) / 100.0,
            updated_at = NOW()
        "#
    )
    .execute(pool)
    .await?;
    
    info!("📈 Updated trending scores for {} NFTs", result.rows_affected());
    Ok(result.rows_affected())
}

/// Calculate similarity between two tag sets (Jaccard similarity)
#[allow(dead_code)]
pub fn calculate_tag_similarity(tags1: &[String], tags2: &[String]) -> f32 {
    if tags1.is_empty() && tags2.is_empty() {
        return 0.5; // Neutral similarity for empty sets
    }
    
    let set1: HashSet<&String> = tags1.iter().collect();
    let set2: HashSet<&String> = tags2.iter().collect();
    
    let intersection = set1.intersection(&set2).count();
    let union = set1.union(&set2).count();
    
    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}
