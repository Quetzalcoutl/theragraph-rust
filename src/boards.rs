//! Board Cache Service
//!
//! Provides high-performance read cache for the boards feature.
//! Reads from the shared PostgreSQL database (Elixir's tables) and caches
//! hot topics and recent posts in memory for sub-millisecond responses.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{error, info};

/// Cache entry with TTL
struct CacheEntry<T> {
    data: T,
    inserted_at: Instant,
    ttl: Duration,
}

impl<T: Clone> CacheEntry<T> {
    fn is_expired(&self) -> bool {
        self.inserted_at.elapsed() > self.ttl
    }
}

/// Board cache state
pub struct BoardCacheState {
    pub pool: PgPool,
    topics_cache: RwLock<Option<CacheEntry<Vec<BoardTopicCached>>>>,
    posts_cache: RwLock<HashMap<String, CacheEntry<Vec<BoardPostCached>>>>,
}

impl BoardCacheState {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            topics_cache: RwLock::new(None),
            posts_cache: RwLock::new(HashMap::new()),
        }
    }
}

// ── Serializable Types ──

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct BoardTopicCached {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub post_count: i32,
    pub last_post_at: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct BoardPostCached {
    pub id: String,
    pub topic_id: String,
    pub author_address: String,
    pub is_incognito: bool,
    pub title: String,
    pub body: String,
    pub media_ipfs_hash: Option<String>,
    pub media_type: Option<String>,
    pub is_pinned: bool,
    pub reply_count: i32,
    pub last_reply_at: Option<chrono::NaiveDateTime>,
    pub inserted_at: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct BoardReplyCached {
    pub id: String,
    pub post_id: String,
    pub parent_reply_id: Option<String>,
    pub author_address: String,
    pub is_incognito: bool,
    pub body: String,
    pub media_ipfs_hash: Option<String>,
    pub media_type: Option<String>,
    pub inserted_at: chrono::NaiveDateTime,
}

#[derive(Debug, Deserialize)]
pub struct BoardPostsQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    25
}

/// Create the board cache router
pub fn board_routes() -> Router<Arc<BoardCacheState>> {
    Router::new()
        .route("/api/v1/boards/topics", get(get_topics))
        .route("/api/v1/boards/topics/:slug/posts", get(get_topic_posts))
        .route("/api/v1/boards/posts/:id", get(get_post_detail))
        .route("/api/v1/boards/posts/:id/replies", get(get_post_replies))
}

/// GET /api/v1/boards/topics — cached list of all board topics
async fn get_topics(
    State(state): State<Arc<BoardCacheState>>,
) -> Result<Json<Vec<BoardTopicCached>>, StatusCode> {
    // Check cache first
    {
        let cache = state.topics_cache.read().await;
        if let Some(entry) = cache.as_ref() {
            if !entry.is_expired() {
                return Ok(Json(entry.data.clone()));
            }
        }
    }

    // Fetch from DB
    let topics: Vec<BoardTopicCached> = sqlx::query_as(
        r#"
        SELECT id::text, slug, name, description, icon, post_count, last_post_at
        FROM board_topics
        WHERE is_active = true
        ORDER BY sort_order ASC, name ASC
        "#,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        error!("Failed to fetch board topics: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Update cache (10 minutes TTL)
    {
        let mut cache = state.topics_cache.write().await;
        *cache = Some(CacheEntry {
            data: topics.clone(),
            inserted_at: Instant::now(),
            ttl: Duration::from_secs(600),
        });
    }

    Ok(Json(topics))
}

/// GET /api/v1/boards/topics/:slug/posts — cached posts for a topic
async fn get_topic_posts(
    State(state): State<Arc<BoardCacheState>>,
    Path(slug): Path<String>,
    Query(params): Query<BoardPostsQuery>,
) -> Result<Json<Vec<BoardPostCached>>, StatusCode> {
    let cache_key = format!("{}:{}:{}", slug, params.limit, params.offset);

    // Check cache (30 second TTL for posts — they update more frequently)
    {
        let cache = state.posts_cache.read().await;
        if let Some(entry) = cache.get(&cache_key) {
            if !entry.is_expired() {
                return Ok(Json(entry.data.clone()));
            }
        }
    }

    let posts: Vec<BoardPostCached> = sqlx::query_as(
        r#"
        SELECT p.id::text, p.topic_id::text, p.author_address, p.is_incognito,
               p.title, p.body, p.media_ipfs_hash, p.media_type, p.is_pinned,
               p.reply_count, p.last_reply_at, p.inserted_at
        FROM board_posts p
        JOIN board_topics t ON t.id = p.topic_id
        WHERE t.slug = $1 AND p.is_blocked = false
        ORDER BY p.is_pinned DESC, p.inserted_at DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(&slug)
    .bind(params.limit)
    .bind(params.offset)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        error!("Failed to fetch board posts for {}: {}", slug, e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Sanitize incognito posts — clear author address
    let posts: Vec<BoardPostCached> = posts
        .into_iter()
        .map(|mut p| {
            if p.is_incognito {
                p.author_address = "anonymous".to_string();
            }
            p
        })
        .collect();

    // Cache for 30 seconds
    {
        let mut cache = state.posts_cache.write().await;
        cache.insert(
            cache_key,
            CacheEntry {
                data: posts.clone(),
                inserted_at: Instant::now(),
                ttl: Duration::from_secs(30),
            },
        );
    }

    Ok(Json(posts))
}

/// GET /api/v1/boards/posts/:id — single post detail
async fn get_post_detail(
    State(state): State<Arc<BoardCacheState>>,
    Path(id): Path<String>,
) -> Result<Json<BoardPostCached>, StatusCode> {
    let post: BoardPostCached = sqlx::query_as(
        r#"
        SELECT id::text, topic_id::text, author_address, is_incognito,
               title, body, media_ipfs_hash, media_type, is_pinned,
               reply_count, last_reply_at, inserted_at
        FROM board_posts
        WHERE id = $1::uuid AND is_blocked = false
        "#,
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        error!("Failed to fetch post {}: {}", id, e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    let mut post = post;
    if post.is_incognito {
        post.author_address = "anonymous".to_string();
    }

    Ok(Json(post))
}

/// GET /api/v1/boards/posts/:id/replies — replies for a post
async fn get_post_replies(
    State(state): State<Arc<BoardCacheState>>,
    Path(id): Path<String>,
    Query(params): Query<BoardPostsQuery>,
) -> Result<Json<Vec<BoardReplyCached>>, StatusCode> {
    let replies: Vec<BoardReplyCached> = sqlx::query_as(
        r#"
        SELECT id::text, post_id::text, parent_reply_id::text,
               author_address, is_incognito, body,
               media_ipfs_hash, media_type, inserted_at
        FROM board_replies
        WHERE post_id = $1::uuid AND is_blocked = false
        ORDER BY inserted_at ASC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(&id)
    .bind(params.limit)
    .bind(params.offset)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        error!("Failed to fetch replies for post {}: {}", id, e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let replies: Vec<BoardReplyCached> = replies
        .into_iter()
        .map(|mut r| {
            if r.is_incognito {
                r.author_address = "anonymous".to_string();
            }
            r
        })
        .collect();

    Ok(Json(replies))
}

/// Warm up board cache on startup
pub async fn warm_cache(state: &Arc<BoardCacheState>) {
    info!("🔥 Warming board cache...");
    match get_topics(State(state.clone())).await {
        Ok(_) => info!("✅ Board topics cache warmed"),
        Err(e) => error!("❌ Failed to warm board topics cache: {:?}", e),
    }
}
