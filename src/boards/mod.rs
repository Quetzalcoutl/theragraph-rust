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

use crate::recommendation::cache::RecCache;

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
    rec_cache: Option<RecCache>,
    topics_cache: RwLock<Option<CacheEntry<Vec<BoardTopicCached>>>>,
    posts_cache: RwLock<HashMap<String, CacheEntry<Vec<BoardPostCached>>>>,
}

impl BoardCacheState {
    #[allow(dead_code)]
    pub fn new(pool: PgPool) -> Self {
        Self::with_cache(pool, None)
    }

    pub fn with_cache(pool: PgPool, rec_cache: Option<RecCache>) -> Self {
        Self {
            pool,
            rec_cache,
            topics_cache: RwLock::new(None),
            posts_cache: RwLock::new(HashMap::new()),
        }
    }
}

// ── Serializable Types ──

// sqlx 0.9's FromRow derive can only decode TEXT columns into `String` — these
// `*Row` types are the DB-decode boundary. The `*Cached` types below (used for
// the in-memory cache, the Redis cache, and the JSON API response — all three
// via serde, which serializes `Arc<str>` identically to `String`) use `Arc<str>`
// instead: `BoardCacheState`'s in-memory cache is read far more often than it's
// written (`get_topics`/`get_topic_posts` clone the *entire* cached Vec on every
// cache-hit request, not just once at insert), so `Arc<str>::clone()` turning
// that into a refcount bump — instead of `String::clone()`'s fresh allocation +
// byte copy for every field of every cached row, every hit — is the actual
// payoff, not just the field's own resting size.

#[derive(sqlx::FromRow)]
struct BoardTopicRow {
    id: String,
    slug: String,
    name: String,
    description: Option<String>,
    icon: Option<String>,
    post_count: i32,
    last_post_at: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardTopicCached {
    pub id: Arc<str>,
    pub slug: Arc<str>,
    pub name: Arc<str>,
    pub description: Option<Arc<str>>,
    pub icon: Option<Arc<str>>,
    pub post_count: i32,
    pub last_post_at: Option<chrono::NaiveDateTime>,
}

impl From<BoardTopicRow> for BoardTopicCached {
    fn from(r: BoardTopicRow) -> Self {
        Self {
            id: r.id.into(),
            slug: r.slug.into(),
            name: r.name.into(),
            description: r.description.map(Into::into),
            icon: r.icon.map(Into::into),
            post_count: r.post_count,
            last_post_at: r.last_post_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct BoardPostRow {
    id: String,
    topic_id: String,
    author_address: String,
    is_incognito: bool,
    title: String,
    body: String,
    media_ipfs_hash: Option<String>,
    media_type: Option<String>,
    is_pinned: bool,
    reply_count: i32,
    last_reply_at: Option<chrono::NaiveDateTime>,
    inserted_at: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardPostCached {
    pub id: Arc<str>,
    pub topic_id: Arc<str>,
    pub author_address: Arc<str>,
    pub is_incognito: bool,
    pub title: Arc<str>,
    pub body: Arc<str>,
    pub media_ipfs_hash: Option<Arc<str>>,
    pub media_type: Option<Arc<str>>,
    pub is_pinned: bool,
    pub reply_count: i32,
    pub last_reply_at: Option<chrono::NaiveDateTime>,
    pub inserted_at: chrono::NaiveDateTime,
}

impl From<BoardPostRow> for BoardPostCached {
    fn from(r: BoardPostRow) -> Self {
        Self {
            id: r.id.into(),
            topic_id: r.topic_id.into(),
            author_address: r.author_address.into(),
            is_incognito: r.is_incognito,
            title: r.title.into(),
            body: r.body.into(),
            media_ipfs_hash: r.media_ipfs_hash.map(Into::into),
            media_type: r.media_type.map(Into::into),
            is_pinned: r.is_pinned,
            reply_count: r.reply_count,
            last_reply_at: r.last_reply_at,
            inserted_at: r.inserted_at,
        }
    }
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
        .route("/api/v1/boards/topics/{slug}/posts", get(get_topic_posts))
        .route("/api/v1/boards/posts/{id}", get(get_post_detail))
        .route("/api/v1/boards/posts/{id}/replies", get(get_post_replies))
}

/// GET /api/v1/boards/topics — cached list of all board topics
async fn get_topics(
    State(state): State<Arc<BoardCacheState>>,
) -> Result<Json<Vec<BoardTopicCached>>, StatusCode> {
    // 1. Redis (warm path when available)
    if let Some(ref rc) = state.rec_cache {
        if let Some(topics) = rc.get_board::<Vec<BoardTopicCached>>("topics").await {
            return Ok(Json(topics));
        }
    }

    // 2. In-memory fallback
    {
        let cache = state.topics_cache.read().await;
        if let Some(entry) = cache.as_ref() {
            if !entry.is_expired() {
                return Ok(Json(entry.data.clone()));
            }
        }
    }

    // 3. DB
    let topics: Vec<BoardTopicCached> = sqlx::query_as::<_, BoardTopicRow>(
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
    })?
    .into_iter()
    .map(BoardTopicCached::from)
    .collect();

    // Write to Redis (10 min TTL) and in-memory
    if let Some(ref rc) = state.rec_cache {
        rc.set_board("topics", &topics, 600).await;
    }
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
    let limit = params.limit.clamp(1, 100);
    let offset = params.offset.max(0).min(100_000);
    let cache_key = format!("posts:{}:{}:{}", slug, limit, offset);

    // 1. Redis
    if let Some(ref rc) = state.rec_cache {
        if let Some(posts) = rc.get_board::<Vec<BoardPostCached>>(&cache_key).await {
            return Ok(Json(posts));
        }
    }

    // 2. In-memory fallback
    {
        let cache = state.posts_cache.read().await;
        if let Some(entry) = cache.get(&cache_key) {
            if !entry.is_expired() {
                return Ok(Json(entry.data.clone()));
            }
        }
    }

    let posts: Vec<BoardPostCached> = sqlx::query_as::<_, BoardPostRow>(
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
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        error!("Failed to fetch board posts for {}: {}", slug, e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .into_iter()
    .map(BoardPostCached::from)
    .collect();

    // Sanitize incognito posts — clear author address
    let posts: Vec<BoardPostCached> = posts
        .into_iter()
        .map(|mut p| {
            if p.is_incognito {
                p.author_address = "anonymous".into();
            }
            p
        })
        .collect();

    // Write to Redis (30s TTL) and in-memory; prune expired entries on write
    if let Some(ref rc) = state.rec_cache {
        rc.set_board(&cache_key, &posts, 30).await;
    }
    {
        const MAX_POSTS_CACHE: usize = 500;
        let mut cache = state.posts_cache.write().await;
        cache.retain(|_, v| !v.is_expired());
        if cache.len() >= MAX_POSTS_CACHE {
            cache.clear();
        }
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
    let post: BoardPostCached = sqlx::query_as::<_, BoardPostRow>(
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
    .ok_or(StatusCode::NOT_FOUND)?
    .into();

    let mut post: BoardPostCached = post;
    if post.is_incognito {
        post.author_address = "anonymous".into();
    }

    Ok(Json(post))
}

/// GET /api/v1/boards/posts/:id/replies — replies for a post
async fn get_post_replies(
    State(state): State<Arc<BoardCacheState>>,
    Path(id): Path<String>,
    Query(params): Query<BoardPostsQuery>,
) -> Result<Json<Vec<BoardReplyCached>>, StatusCode> {
    let limit = params.limit.clamp(1, 100);
    let offset = params.offset.max(0).min(100_000);

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
    .bind(limit)
    .bind(offset)
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


#[cfg(test)]
mod tests;
