//! Redis key builders and cache TTL constants.
//!
//! All Redis keys for the recommendation namespace flow through `CacheKey::*`
//! so prefix strings live in exactly one place. The `PREFIX_*` constants are
//! private — only the constructors here can build keys, preventing silent
//! mismatches caused by copy-pasted string literals in callers.

use std::sync::Arc;

use super::super::graph_client::FofBucket;
use super::super::schema_consts::REDIS_TOPIC_AFFINITY_PREFIX;

// ── TTL constants (seconds) ───────────────────────────────────────────────────

pub const NEBULA_QUERY_TTL: u64 = 300;       // 5 min — graph traversals change slowly
pub const NFT_FEATURES_TTL: u64 = 1800;      // 30 min — features are near-immutable
pub const USER_PREFS_TTL: u64 = 300;          // 5 min — preferences shift slowly
pub const FOLLOWING_TTL: u64 = 300;           // 5 min — follow graph changes infrequently
pub const SEEN_NFTS_TTL: u64 = 1800;         // 30 min — seen set for dedup
pub const RECOMMENDATION_TTL: u64 = 600;     // 10 min — cached recommendations
pub const SESSION_TTL: u64 = 7200;           // 2h — YouTube-style session recency window
pub const STYLE_PREFS_TTL: u64 = 1800;       // 30 min — companion style_preference edges change on chat, not per-request
/// 30 min — same rationale as STYLE_PREFS_TTL: declared genre_preference edges
/// (written via `POST /api/v1/genre-preferences/{addr}`) change on explicit
/// user action, not per-request.
pub const GENRE_PREFS_TTL: u64 = 1800;
/// 5 min — GENRE-03: TTL for the shared, non-user-keyed genre-tagged
/// candidate pool (`CacheKey::genre_pool`). Mirrors `NEBULA_QUERY_TTL`'s
/// rationale: the underlying Postgres query (`nft_features.tags && $genre`)
/// changes slowly relative to request volume — new genre-tagged NFTs appear
/// far less often than the pool is read — and a stale entry is harmless
/// (self-heals on next expiry, same as every other cache tier here).
pub const GENRE_POOL_TTL: u64 = 300;

// ── Key prefixes for namespace isolation ──────────────────────────────────────

const PREFIX_NEBULA: &str = "rec:nebula:";
/// NASA-3: bumped to v2 after C3 changed the cached type from NftFeatures (11 fields)
/// to ScoringFeatures (4 fields).
/// Bumped again to v3 (session U2026-09-04): wire format changed from JSON to
/// MessagePack (`RecCache::encode`/`decode` in cache/ops.rs) — old JSON-text
/// entries would fail `rmp_serde` decode as garbage bytes rather than cleanly
/// miss, so every prefix that round-trips through `get_json`/`set_json` (or,
/// for `PREFIX_SESSION`, the LPUSH/LRANGE `encode`/`decode` pair) bumped in the
/// same pass: features v2→v3, prefs/recs/following/board/session v1→v2, plus
/// the FoF bucket prefixes in `FofBucket::prefix()` and the inline
/// `rec:suggestions:` key. Old-version entries simply miss (→ DB fetch →
/// store new-version entry); no manual flush required, they expire naturally.
/// Bump this version any time the serialised shape OR wire format of a cached
/// type changes.
const PREFIX_FEATURES: &str = "rec:features:v3:";
const PREFIX_PREFS: &str = "rec:prefs:v2:";
const PREFIX_FOLLOWING: &str = "rec:following:v2:";
const PREFIX_SEEN: &str = "rec:seen:";
const PREFIX_RECS: &str = "rec:results:v2:";
// FoF Redis key prefixes live in FofBucket::prefix() — not duplicated here.
const PREFIX_BOARD: &str = "board:v2:";
const PREFIX_SESSION: &str = "rec:session:v2:";
const PREFIX_STYLE_PREFS: &str = "rec:style_prefs:v1:";
const PREFIX_GENRE_PREFS: &str = "rec:genre_prefs:v1:";
const PREFIX_GENRE_POOL: &str = "rec:genre_pool:v1:";

// ── Domain type ───────────────────────────────────────────────────────────────

/// One user interaction recorded in the session recency list.
///
/// Stored as MessagePack in a Redis LIST (LPUSH + LTRIM to cap at 20 entries).
/// Scoring reads these to compute a short-term recency boost — `weight ×
/// exp(-age_secs / 1800)` — that layers on top of long-term preferences.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionSignal {
    // Arc<str>, interned via tag_interner: same NFT-tag vocabulary as
    // ScoredNft/ScoringFeatures.tags, and this struct is built fresh on every
    // like/comment/view interaction event (not just once per feed serve) — the
    // same popular tags recur across users' interactions far more often than
    // per-feed alone, so sharing one allocation per tag matters here too.
    pub tags: Box<[Arc<str>]>,
    pub creator: Option<Box<str>>,
    pub interaction_weight: f32,
    pub ts_unix: i64,
}

// ── Key constructors ──────────────────────────────────────────────────────────

/// Centralized cache key constructors.
///
/// Every Redis key in the recommendation namespace is built here so that
/// prefix strings live in exactly one place. Callers import `CacheKey` and
/// call e.g. `CacheKey::fof_bucket(FofBucket::FollowLike, addr)` — a typo in a prefix is now a compile
/// error rather than a silent cache miss.
pub struct CacheKey;

impl CacheKey {
    /// Key for any FoF bucket — single constructor parameterised by bucket type.
    ///
    /// Replaces the 6 per-bucket methods (fof/fof_view/fof_comment/…).
    /// Prefix is owned by `FofBucket::prefix()` so adding a new bucket
    /// only requires a new enum variant, not a new constant + new method here.
    pub(crate) fn fof_bucket(bucket: FofBucket, addr: &str) -> String {
        format!("{}{}", bucket.prefix(), addr.to_lowercase())
    }

    /// Vertex-seen bloom-filter key.
    ///
    /// Set with SETNX + 24h TTL when a vertex is first upserted into Nebula.
    /// When this key exists, the vertex INSERT-IF-NOT-EXISTS nGQL fragment is
    /// elided from the query — saving one Nebula IF-NOT-EXISTS round-trip per
    /// write event for hot (viral) vertices. False positives (key present but
    /// vertex gone) are harmless: the missing vertex is detected on the next
    /// Nebula query path and auto-created by the next cold-path upsert.
    pub fn vertex_seen(vid: &str) -> String {
        format!("vx:{}", vid)
    }

    /// Key for "Who to Follow" user suggestion results.
    /// Scoped by viewer + creator so each profile page gets its own cache slot.
    pub fn user_suggestions(viewer: &str, creator: &str) -> String {
        format!("rec:suggestions:v2:{}:{}", viewer.to_lowercase(), creator.to_lowercase())
    }

    /// Key for a cached Nebula NGQL query result (keyed by hash).
    pub fn nebula(hash: &str) -> String {
        format!("{}{}", PREFIX_NEBULA, hash)
    }

    /// Key for cached NFT feature vectors.
    pub fn features(nft_id: &str) -> String {
        format!("{}{}", PREFIX_FEATURES, nft_id)
    }

    /// Key for cached user preferences.
    pub fn prefs(addr: &str) -> String {
        format!("{}{}", PREFIX_PREFS, addr.to_lowercase())
    }

    /// Key for cached recommendation results, scoped by feed type.
    pub fn recs(addr: &str, feed_type: &str) -> String {
        format!("{}{}:{}", PREFIX_RECS, addr.to_lowercase(), feed_type)
    }

    /// Key for cached following-address list.
    pub fn following(addr: &str) -> String {
        format!("{}{}", PREFIX_FOLLOWING, addr.to_lowercase())
    }

    /// Key for the seen-NFT set (Redis SET used for dedup).
    pub fn seen(addr: &str) -> String {
        format!("{}{}", PREFIX_SEEN, addr.to_lowercase())
    }

    /// Key for board cache entries.
    pub fn board(key: &str) -> String {
        format!("{}{}", PREFIX_BOARD, key)
    }

    /// Key for the session-recency signal list (Redis LIST, capped at 20 entries).
    pub fn session(addr: &str) -> String {
        format!("{}{}", PREFIX_SESSION, addr.to_lowercase())
    }

    /// Key for a per-user per-tag topic-affinity score written by Elixir.
    ///
    /// Both address and tag are lowercased so keys are stable regardless of
    /// input casing. All `topic_affinity:` keys live in one place here — a
    /// future prefix rename is a one-line change with no risk of mismatched
    /// call sites.
    pub fn topic_affinity(addr: &str, tag: &str) -> String {
        format!("{}:{}:{}", REDIS_TOPIC_AFFINITY_PREFIX, addr.to_lowercase(), tag.to_lowercase())
    }

    /// Key for cached style_preference Nebula-traversal results (companion
    /// chat signal — see `graph_client::get_style_preferences`).
    pub fn style_prefs(addr: &str) -> String {
        format!("{}{}", PREFIX_STYLE_PREFS, addr.to_lowercase())
    }

    /// Key for cached genre_preference Nebula-traversal results (declared
    /// listener taste — see `graph_client::get_genre_preferences`).
    pub fn genre_prefs(addr: &str) -> String {
        format!("{}{}", PREFIX_GENRE_PREFS, addr.to_lowercase())
    }

    /// Key for the shared, non-user-keyed genre-tagged candidate pool
    /// (GENRE-03) — `genre_hash` is the same `genre_feed_type(genre_slugs)`
    /// hash used for the per-user recommendation-result cache, reused here
    /// verbatim (no separate hash needed: two users requesting the same slug
    /// set already collapse onto the same hash by construction).
    ///
    /// Deliberately a *separate* key from [`Self::recs`] even though both are
    /// keyed by the same hash: `recs(addr, genre_feed_type(..))` holds one
    /// user's fully-scored, fully-personalized *output* (post prefs/session/
    /// FoF boosts, post seen/not-interested filtering, post diversity
    /// shuffle); this key holds the raw, unscored `Vec<CandidateNft>` *input*
    /// that `list_affinity_candidates(genre_slugs, &[], ..)` — a pure
    /// function of the slug set alone, no per-user state — would otherwise
    /// recompute via an identical Postgres query for every user who shares
    /// that genre combination.
    pub fn genre_pool(genre_hash: &str) -> String {
        format!("{}{}", PREFIX_GENRE_POOL, genre_hash)
    }
}

/// Deterministic, order/case-insensitive `feed_type` suffix for a
/// genre-filtered recommendation cache entry (GENRE-02 follow-up).
///
/// `RecommendationEngine::get_recommendations`'s `genre_slugs` parameter
/// serves two different call shapes — a user's full declared-favorites set
/// (stable, up to `MAX_GENRE_SLUGS` slugs) and a single one-off "radio" slug
/// (`useGenreRadio.ts`, varies per request) — both need a cache key that
/// depends on the actual slug *content*, not just `user_address`, since two
/// different slug sets legitimately produce two different valid results for
/// the same user (`CacheKey::recs(addr, &genre_feed_type(slugs))`).
///
/// Slugs are trimmed, lowercased, deduped and sorted before hashing, so the
/// same *set* of slugs always maps to the same key regardless of the order or
/// casing supplied — this lets the hourly pre-warm job
/// (`RecommendationEngine::warmup_genre_feed`) and a live "For You" request
/// land on the same cache entry even though each reconstructs the slug list
/// independently (Nebula edge enumeration vs. a frontend-supplied list).
///
/// Truncated to 8 hex chars (32 bits): `recommendation_cache.feed_type` is
/// `VARCHAR(20)` and `"genre:" + 16 hex chars` would overflow it. A collision
/// only means one user's genre combination briefly reuses another of their
/// own combinations' cached results — it self-heals on the next write/TTL
/// expiry, not worth a schema migration to widen the column.
pub fn genre_feed_type(genre_slugs: &[String]) -> String {
    let mut normalized: Vec<String> = genre_slugs
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    normalized.sort_unstable();
    normalized.dedup();

    // FNV-1a — same algorithm as `RecCache::hash_query` (cache/ops.rs), kept
    // as a separate copy since that one is `pub(super)` to the cache module
    // and this is a pure, no-I/O helper called from `engine` and `api`
    // without needing a `RecCache` handle.
    let mut hash: u64 = 0xcbf29ce484222325;
    for slug in &normalized {
        for byte in slug.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        // Separator so e.g. ["ab", "c"] and ["a", "bc"] don't collide.
        // 0xff never appears in a slug byte — slugs are restricted to
        // lowercase ascii/digits/hyphen at the API boundary.
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("genre:{:08x}", hash as u32)
}
