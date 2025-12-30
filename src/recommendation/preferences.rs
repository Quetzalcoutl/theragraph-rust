//! User Preferences and Interaction Tracking
//!
//! Tracks user behavior to build personalized preference profiles.
//! Updates preferences based on likes, purchases, views, and other interactions.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::info;

/// Interaction types we track
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InteractionType {
    View,
    Like,
    Unlike,
    Comment,
    Purchase,
    Share,
    Save,
    Unsave,
}

impl std::fmt::Display for InteractionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InteractionType::View => write!(f, "view"),
            InteractionType::Like => write!(f, "like"),
            InteractionType::Unlike => write!(f, "unlike"),
            InteractionType::Comment => write!(f, "comment"),
            InteractionType::Purchase => write!(f, "purchase"),
            InteractionType::Share => write!(f, "share"),
            InteractionType::Save => write!(f, "save"),
            InteractionType::Unsave => write!(f, "unsave"),
        }
    }
}

/// User preference profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPreferences {
    pub user_address: String,

    // Content type affinities (0.0 to 1.0)
    pub snap_affinity: f32,
    pub art_affinity: f32,
    pub music_affinity: f32,
    pub flix_affinity: f32,

    // Tag preferences: tag -> weight
    pub tag_preferences: HashMap<String, f32>,

    // Creator preferences: address -> weight
    pub creator_preferences: HashMap<String, f32>,

    // Behavioral stats
    pub total_likes: i32,
    pub total_purchases: i32,
    pub total_views: i32,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            user_address: String::new(),
            snap_affinity: 0.5,
            art_affinity: 0.5,
            music_affinity: 0.5,
            flix_affinity: 0.5,
            tag_preferences: HashMap::new(),
            creator_preferences: HashMap::new(),
            total_likes: 0,
            total_purchases: 0,
            total_views: 0,
        }
    }
}

/// Interaction event for recording
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionEvent {
    pub user_address: String,
    pub nft_id: String,
    pub interaction_type: InteractionType,
    pub view_duration_ms: Option<i64>,
    pub source: Option<String>,
    pub nft_contract_type: Option<String>,
    pub nft_creator_address: Option<String>,
    pub nft_tags: Vec<String>,
}

/// Preference learning weights
const LIKE_WEIGHT: f32 = 1.0;
const PURCHASE_WEIGHT: f32 = 3.0; // Purchases are strongest signal
const VIEW_WEIGHT: f32 = 0.1; // Views are weak signal
const LONG_VIEW_WEIGHT: f32 = 0.3; // Long views (>5s) are stronger
const UNLIKE_WEIGHT: f32 = -0.5; // Negative signal
const DECAY_FACTOR: f32 = 0.95; // Daily decay for old preferences
const LONG_VIEW_THRESHOLD_MS: i64 = 5000;

/// Records a user interaction and updates preferences
pub async fn record_interaction(pool: &PgPool, event: InteractionEvent) -> Result<()> {
    // 1. Insert interaction record
    insert_interaction(pool, &event).await?;

    // 2. Update user preferences based on interaction
    update_preferences_from_interaction(pool, &event).await?;

    info!(
        "📊 Recorded {} interaction: user={}, nft={}",
        event.interaction_type, event.user_address, event.nft_id
    );

    Ok(())
}

async fn insert_interaction(pool: &PgPool, event: &InteractionEvent) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO user_interactions 
            (id, user_address, nft_id, interaction_type, view_duration_ms, source,
             nft_contract_type, nft_creator_address, nft_tags, created_at)
        VALUES 
            (gen_random_uuid(), $1, $2::uuid, $3, $4, $5, $6, $7, $8, NOW())
        "#,
    )
    .bind(&event.user_address)
    .bind(&event.nft_id)
    .bind(event.interaction_type.to_string())
    .bind(event.view_duration_ms)
    .bind(&event.source)
    .bind(&event.nft_contract_type)
    .bind(&event.nft_creator_address)
    .bind(&event.nft_tags)
    .execute(pool)
    .await?;

    Ok(())
}

async fn update_preferences_from_interaction(
    pool: &PgPool,
    event: &InteractionEvent,
) -> Result<()> {
    // Calculate weight based on interaction type
    let weight = match event.interaction_type {
        InteractionType::Like => LIKE_WEIGHT,
        InteractionType::Comment => LIKE_WEIGHT * 0.8,
        InteractionType::Purchase => PURCHASE_WEIGHT,
        InteractionType::View => {
            if event.view_duration_ms.unwrap_or(0) > LONG_VIEW_THRESHOLD_MS {
                LONG_VIEW_WEIGHT
            } else {
                VIEW_WEIGHT
            }
        }
        InteractionType::Unlike => UNLIKE_WEIGHT,
        InteractionType::Unsave => UNLIKE_WEIGHT * 0.5,
        InteractionType::Share => LIKE_WEIGHT * 0.5,
        InteractionType::Save => LIKE_WEIGHT * 0.7,
    };

    // Get or create user preferences
    let mut prefs = get_or_create_preferences(pool, &event.user_address).await?;

    // Update content type affinity
    if let Some(ref contract_type) = event.nft_contract_type {
        update_content_affinity(&mut prefs, contract_type, weight);
    }

    // Update tag preferences
    for tag in &event.nft_tags {
        let current = prefs.tag_preferences.get(tag).copied().unwrap_or(0.5);
        let new_value = (current + weight * 0.1).clamp(0.0, 1.0);
        prefs.tag_preferences.insert(tag.clone(), new_value);
    }

    // Update creator preferences
    if let Some(ref creator) = event.nft_creator_address {
        let current = prefs
            .creator_preferences
            .get(creator)
            .copied()
            .unwrap_or(0.5);
        let new_value = (current + weight * 0.1).clamp(0.0, 1.0);
        prefs.creator_preferences.insert(creator.clone(), new_value);
    }

    // Update behavioral stats
    match event.interaction_type {
        InteractionType::Like => prefs.total_likes += 1,
        InteractionType::Comment => prefs.total_likes += 1, // Comments also count as engagement
        InteractionType::Purchase => prefs.total_purchases += 1,
        InteractionType::View => prefs.total_views += 1,
        _ => {}
    }

    // Save updated preferences
    save_preferences(pool, &prefs).await?;

    Ok(())
}

fn update_content_affinity(prefs: &mut UserPreferences, contract_type: &str, weight: f32) {
    let delta = weight * 0.05; // Small increments

    match contract_type.to_lowercase().as_str() {
        "snap" => prefs.snap_affinity = (prefs.snap_affinity + delta).clamp(0.0, 1.0),
        "art" => prefs.art_affinity = (prefs.art_affinity + delta).clamp(0.0, 1.0),
        "music" => prefs.music_affinity = (prefs.music_affinity + delta).clamp(0.0, 1.0),
        "flix" => prefs.flix_affinity = (prefs.flix_affinity + delta).clamp(0.0, 1.0),
        _ => {}
    }
}

/// Database row for preferences
#[derive(Debug, sqlx::FromRow)]
struct PreferencesRow {
    user_address: String,
    snap_affinity: f32,
    art_affinity: f32,
    music_affinity: f32,
    flix_affinity: f32,
    tag_preferences: serde_json::Value,
    creator_preferences: serde_json::Value,
    total_likes: i32,
    total_purchases: i32,
    total_views: i32,
}

pub async fn get_or_create_preferences(
    pool: &PgPool,
    user_address: &str,
) -> Result<UserPreferences> {
    let normalized = user_address.to_lowercase();

    let result = sqlx::query_as::<_, PreferencesRow>(
        r#"
        SELECT 
            user_address, snap_affinity, art_affinity, music_affinity, flix_affinity,
            tag_preferences, creator_preferences, total_likes, total_purchases, total_views
        FROM user_preferences
        WHERE user_address = $1
        "#,
    )
    .bind(&normalized)
    .fetch_optional(pool)
    .await?;

    match result {
        Some(row) => Ok(UserPreferences {
            user_address: row.user_address,
            snap_affinity: row.snap_affinity,
            art_affinity: row.art_affinity,
            music_affinity: row.music_affinity,
            flix_affinity: row.flix_affinity,
            tag_preferences: serde_json::from_value(row.tag_preferences).unwrap_or_default(),
            creator_preferences: serde_json::from_value(row.creator_preferences)
                .unwrap_or_default(),
            total_likes: row.total_likes,
            total_purchases: row.total_purchases,
            total_views: row.total_views,
        }),
        None => {
            // Create new preferences with defaults
            let prefs = UserPreferences {
                user_address: normalized.clone(),
                ..Default::default()
            };

            let tag_prefs_json = serde_json::to_value(&prefs.tag_preferences)?;
            let creator_prefs_json = serde_json::to_value(&prefs.creator_preferences)?;

            sqlx::query(
                r#"
                INSERT INTO user_preferences 
                    (id, user_address, snap_affinity, art_affinity, music_affinity, flix_affinity,
                     tag_preferences, creator_preferences, total_likes, total_purchases, total_views,
                     last_activity_at, inserted_at, updated_at)
                VALUES 
                    (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW(), NOW())
                ON CONFLICT (user_address) DO NOTHING
                "#
            )
            .bind(&normalized)
            .bind(prefs.snap_affinity)
            .bind(prefs.art_affinity)
            .bind(prefs.music_affinity)
            .bind(prefs.flix_affinity)
            .bind(&tag_prefs_json)
            .bind(&creator_prefs_json)
            .bind(prefs.total_likes)
            .bind(prefs.total_purchases)
            .bind(prefs.total_views)
            .execute(pool)
            .await?;

            Ok(prefs)
        }
    }
}

async fn save_preferences(pool: &PgPool, prefs: &UserPreferences) -> Result<()> {
    let tag_prefs_json = serde_json::to_value(&prefs.tag_preferences)?;
    let creator_prefs_json = serde_json::to_value(&prefs.creator_preferences)?;

    sqlx::query(
        r#"
        UPDATE user_preferences SET
            snap_affinity = $2,
            art_affinity = $3,
            music_affinity = $4,
            flix_affinity = $5,
            tag_preferences = $6,
            creator_preferences = $7,
            total_likes = $8,
            total_purchases = $9,
            total_views = $10,
            last_activity_at = NOW(),
            updated_at = NOW()
        WHERE user_address = $1
        "#,
    )
    .bind(&prefs.user_address)
    .bind(prefs.snap_affinity)
    .bind(prefs.art_affinity)
    .bind(prefs.music_affinity)
    .bind(prefs.flix_affinity)
    .bind(&tag_prefs_json)
    .bind(&creator_prefs_json)
    .bind(prefs.total_likes)
    .bind(prefs.total_purchases)
    .bind(prefs.total_views)
    .execute(pool)
    .await?;

    Ok(())
}

/// Apply time decay to all preferences (run daily via cron)
pub async fn apply_preference_decay(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE user_preferences SET
            snap_affinity = 0.5 + (snap_affinity - 0.5) * $1,
            art_affinity = 0.5 + (art_affinity - 0.5) * $1,
            music_affinity = 0.5 + (music_affinity - 0.5) * $1,
            flix_affinity = 0.5 + (flix_affinity - 0.5) * $1,
            updated_at = NOW()
        WHERE last_activity_at < NOW() - INTERVAL '1 day'
        "#,
    )
    .bind(DECAY_FACTOR as f64)
    .execute(pool)
    .await?;

    info!(
        "🔄 Applied preference decay to {} users",
        result.rows_affected()
    );
    Ok(result.rows_affected())
}
