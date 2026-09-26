//! Schema constants — single source of truth for the Nebula space name,
//! edge names, property names, and VID constructors used in nGQL format strings.
//!
//! Every `format!()` call that embeds a Nebula space name, edge name, or a
//! `"user:{addr}"` / `"post:{id}"` VID should reference the items here so a
//! future rename (or FIXED_STRING capacity change) touches exactly one file.
//!
//! # VID bounds
//! NebulaGraph declares VIDs as `FIXED_STRING(128)`. The `vid_user` and
//! `vid_post` helpers assert at debug-build time that the resulting string
//! stays within that bound, catching oversized inputs at write time rather
//! than letting Nebula silently truncate them.

// ── Space ─────────────────────────────────────────────────────────────────────

/// NebulaGraph space that owns all TheraGraph vertices and edges.
pub const SPACE_THERAGRAPH: &str = "theragraph";

// ── Schema version gate ───────────────────────────────────────────────────────

/// Expected Nebula schema version — must match the `schema_version` property on
/// the `"schema_meta"` vertex written by `init-entrypoint.sh` as its final step.
///
/// Bump this constant whenever a new migration file is added to
/// `theragraph-nebula/init/`. At Rust startup `check_nebula_schema_version`
/// reads the vertex and fails fast with a clear message if the versions differ.
///
/// Current schema: migrations 01-26 applied (purchases edge, schema_meta TAG,
/// drop zombie indexes, bookmarked + shared edges, comments_on prune index,
/// music_listen edge, style_preference + genre_preference edges for companion signals,
/// flix_watch edge for video completion + rewatch signals,
/// flix_watch_time_index + flix_watch_weight_index for FoF traversal performance,
/// likes.reaction_type + post.content_type narrowed from unconstrained `string`
/// to `fixed_string(20)` — small closed vocabularies, matches the existing
/// post_type_time_index(content_type(20), ...) prefix length,
/// vid_type shrunk FIXED_STRING(128) -> FIXED_STRING(64) — measured against
/// every id column in the Postgres schema (all uuid/bigint, true max 47 bytes
/// for "user:<addr>") with 17 bytes of margin. `VID_MAX_LEN` below must match.
/// Migration 26 (lazy-mint economy, 2026-09-13): `post` tag gains `price`
/// (double, human-scale TREEZ — see `wei_str_to_treez` at the write path),
/// `max_copies` (int64, 0 = unlimited, matches the contract's own convention),
/// `copies_minted` (int64 DEFAULT 0) — additive `ALTER TAG ... ADD`, no
/// drop/recreate needed since these are genuinely new property names (unlike
/// migration 24's `reaction_type`/`content_type`, which hit NebulaGraph v3's
/// permanent "can't re-add a dropped property name" wall).
pub const NEBULA_SCHEMA_VERSION: u32 = 26;

/// Parse the `version` integer out of the ASCII table `nebula-console` prints
/// for `FETCH PROP ON schema_meta ... YIELD properties(vertex).version`.
///
/// Lives in the library (not inlined in `main.rs`'s binary crate) so
/// integration tests in `tests/` can call the real parser instead of
/// re-typing its logic — a prior version of this function existed only in
/// `main.rs`, unreachable from `tests/pipeline_integration.rs`, so the tests
/// there duplicated the parsing closure character-for-character. That meant
/// editing the real parser and forgetting the test copy would leave the test
/// suite passing against stale logic forever — this had already happened.
///
/// Expected table shape:
/// ```text
/// +---------+
/// | version |
/// +---------+
/// | 19      |
/// +---------+
/// ```
pub fn parse_schema_version_table(output: &str) -> Option<u32> {
    output
        .lines()
        .find(|l| {
            let t = l.trim();
            t.starts_with('|') && !t.contains("version") && !t.starts_with('+')
        })
        .and_then(|l| l.split('|').nth(1))
        .and_then(|v| v.trim().parse::<u32>().ok())
}

// ── Edge names ────────────────────────────────────────────────────────────────

pub const EDGE_FOLLOWS: &str = "follows";
pub const EDGE_LIKES: &str = "likes";
/// Dedicated purchases edge (migration 15). Enables storaged-level selectivity
/// in get_purchase_fof_recommendations — no graphd-side WHERE filter needed.
/// Schema: (event_id string, purchased_at timestamp, weight double DEFAULT 2.0).
pub const EDGE_PURCHASES: &str = "purchases";
pub const EDGE_VIEW_EVENT: &str = "view_event";
pub const EDGE_CREATOR_AFFINITY: &str = "creator_affinity";
pub const EDGE_RECOMMENDED_TO: &str = "recommended_to";
pub const EDGE_COMMENTS_ON: &str = "comments_on";
/// TAG-S29-04: bookmark and share edges — highest-intent signals missing from the
/// recommendation graph.  Schema defined in migration 009.
/// bookmarked: (event_id string, bookmarked_at timestamp) — IF NOT EXISTS (toggle-safe).
/// shared:     (event_id string, shared_at timestamp, rank derived from tx_hash).
pub const EDGE_BOOKMARKED: &str = "bookmarked";
pub const EDGE_SHARED: &str = "shared";
/// Music listen edge — distinct from view_event (feed scroll) so the graph can
/// apply music-specific half-life (10 days, same as bookmarked) and FoF traversal.
/// UPSERT semantics: one edge per (listener, post) pair; duration_seconds and
/// pct_played accumulate across repeated listens (geometric mean for pct_played).
/// Schema defined in migration 020 (CREATE EDGE IF NOT EXISTS music_listen):
///   event_id STRING, listened_at DATETIME, duration_seconds INT32,
///   pct_played FLOAT DEFAULT 0.0, weight FLOAT DEFAULT 1.0
pub const EDGE_MUSIC_LISTEN: &str = "music_listen";
/// Flix watch edge — distinct from view_event (feed scroll) so the graph can
/// apply flix-specific half-life and FoF traversal using completion rate.
/// Schema defined in migration 022 (CREATE EDGE IF NOT EXISTS flix_watch):
///   event_id STRING, watched_at DATETIME, duration_seconds INT32,
///   pct_played FLOAT DEFAULT 0.0, rewatch_count INT32, weight FLOAT DEFAULT 1.0
pub const EDGE_FLIX_WATCH: &str = "flix_watch";
pub const PROP_WATCHED_AT: &str = "watched_at";
pub const PROP_REWATCH_COUNT: &str = "rewatch_count";
// ── Property names ────────────────────────────────────────────────────────────
// Single source of truth for every Nebula property name used in nGQL format
// strings.  All constants are substituted via named-argument syntax so a future
// schema rename touches only this file.  No #[allow(dead_code)] — each constant
// must appear as a named arg in at least one format!/write! call; the compiler
// will flag any addition to the catalog that is not yet wired up.
pub const PROP_DURATION_SECONDS: &str = "duration_seconds";
pub const PROP_WEIGHT: &str = "weight";
pub const PROP_SCORE: &str = "score";
pub const PROP_SERVED: &str = "served";
pub const PROP_EVENT_ID: &str = "event_id";
pub const PROP_FOLLOWED_AT: &str = "followed_at";
pub const PROP_LIKED_AT: &str = "liked_at";
pub const PROP_COMMENTED_AT: &str = "commented_at";
pub const PROP_COMPUTED_AT: &str = "computed_at";
pub const PROP_EVENT_TIME: &str = "event_time";
pub const PROP_TOTAL_VIEWS: &str = "total_views";
pub const PROP_TOTAL_DURATION_SECS: &str = "total_duration_secs";
pub const PROP_AFFINITY_SCORE: &str = "affinity_score";
pub const PROP_LAST_INTERACTION_AT: &str = "last_interaction_at";
pub const PROP_COMMENT_TEXT: &str = "comment_text";
pub const PROP_REACTION_TYPE: &str = "reaction_type";
/// Property name for the `purchases` edge timestamp (migration 15).
pub const PROP_PURCHASED_AT: &str = "purchased_at";
/// TAG-S29-04: bookmark/share edge timestamp properties (migration 009).
pub const PROP_BOOKMARKED_AT: &str = "bookmarked_at";
pub const PROP_SHARED_AT: &str = "shared_at";
/// Music listen edge properties (migration 020).
pub const PROP_PCT_PLAYED: &str = "pct_played";
pub const PROP_LISTENED_AT: &str = "listened_at";
/// Style/genre preference edge properties (migration 021).
pub const EDGE_STYLE_PREFERENCE: &str = "style_preference";
pub const EDGE_GENRE_PREFERENCE: &str = "genre_preference";
pub const PROP_CONFIDENCE: &str = "confidence";
pub const PROP_TIME_CONTEXT: &str = "time_context";
pub const PROP_SIGNAL_SOURCE: &str = "source";

// ── Vertex property names ─────────────────────────────────────────────────────
// Pre-declared for INSERT VERTEX / UPDATE VERTEX paths not yet implemented in
// Rust (currently done via Elixir). Suppressed per-constant so new wired
// additions don't accidentally inherit the suppression.
#[allow(dead_code)] pub const PROP_ID: &str = "id";
#[allow(dead_code)] pub const PROP_USERNAME: &str = "username";
#[allow(dead_code)] pub const PROP_FOLLOWERS_COUNT: &str = "followers_count";
#[allow(dead_code)] pub const PROP_FOLLOWING_COUNT: &str = "following_count";
#[allow(dead_code)] pub const PROP_TOTAL_LIKES_GIVEN: &str = "total_likes_given";
#[allow(dead_code)] pub const PROP_TOTAL_POSTS: &str = "total_posts";
#[allow(dead_code)] pub const PROP_CONTENT: &str = "content";
#[allow(dead_code)] pub const PROP_AUTHOR_ID: &str = "author_id";
#[allow(dead_code)] pub const PROP_VIEWS: &str = "views";
#[allow(dead_code)] pub const PROP_LIKES: &str = "likes";
#[allow(dead_code)] pub const PROP_HASHTAGS: &str = "hashtags";
#[allow(dead_code)] pub const PROP_CONTENT_TYPE: &str = "content_type";
/// `post` tag properties added by migration 26 (lazy-mint economy, 2026-09-13).
/// Wired up in `graph_sync::content_ops` (ContentMinted/ListingUpdated) and
/// `graph_sync::social_ops::inner_sync_purchase` (copies_minted increment on
/// ContentCopyMinted) — no `#[allow(dead_code)]` since these are actively used.
pub const PROP_PRICE: &str = "price";
pub const PROP_MAX_COPIES: &str = "max_copies";
pub const PROP_COPIES_MINTED: &str = "copies_minted";

// ── Feed type constants ───────────────────────────────────────────────────────
// Single source of truth for the feed-type string used as a Redis key segment
// (rec:results:{addr}:{feed_type}) and as the `feed_type` column in the
// `recommendation_cache` Postgres table.  All callers import these constants so
// a rename touches exactly one file and never silently creates a cache miss.

pub const FEED_TYPE_PERSONALIZED: &str = "personalized";
pub const FEED_TYPE_ENHANCED: &str = "enhanced";
pub const FEED_TYPE_TRENDING: &str = "trending";
pub const FEED_TYPE_FOLLOWING: &str = "following";

// ── VID constructors ──────────────────────────────────────────────────────────

/// FIXED_STRING(64) capacity for VIDs in the theragraph space (migration 25 —
/// shrunk from 128; true measured max is 47 bytes for "user:<42-char-addr>",
/// this constant MUST track the space's actual `vid_type` or the overflow
/// guards below stop meaning anything).
const VID_MAX_LEN: usize = 64;

/// Build a user VID: `"user:{addr}"`.
///
/// Asserts in debug builds that `"user:" + addr` fits within FIXED_STRING(128),
/// catching oversized inputs at the write-path boundary instead of silently
/// truncating them inside Nebula.
///
/// In production Ethereum addresses (0x + 40 hex = 42 bytes + 5-byte prefix = 47)
/// the assertion never fires — it is a guard against future data-shape changes.
#[inline]
pub fn vid_user(addr: &str) -> String {
    debug_assert!(
        addr.len() + 5 <= VID_MAX_LEN,
        "user VID too long: 'user:{}' ({} bytes) exceeds FIXED_STRING(128)",
        addr,
        addr.len() + 5,
    );
    format!("user:{addr}")
}

/// Build a genre VID: `"genre:{slug}"`.
///
/// `slug` must already be sanitized (see `sanitize_genre_slug`) — this function
/// does not re-validate, matching `vid_user`'s contract where the caller owns
/// input safety. Replaces the old opaque `format!("genre:{genre_id}")` numeric-id
/// VID format: the canonical genre identifier across the whole system (NFT
/// content tags, listener-declared preferences, and companion-signal writes) is
/// now a kebab-case slug string, never a numeric id.
#[inline]
pub fn vid_genre(slug: &str) -> String {
    debug_assert!(
        slug.len() + 6 <= VID_MAX_LEN,
        "genre VID too long: 'genre:{}' ({} bytes) exceeds FIXED_STRING({VID_MAX_LEN})",
        slug,
        slug.len() + 6,
    );
    format!("genre:{slug}")
}

/// Normalize free text (or a frontend-supplied slug) into a safe kebab-case
/// genre slug for embedding in a Nebula VID.
///
/// Lowercases, collapses any run of whitespace/underscore/slash/`&`/`+` into a
/// single `-` (word-joining punctuation becomes a separator, e.g. "R&B" ->
/// "r-b", rather than being dropped and mashing words together), drops every
/// other non `[a-z0-9-]` character, collapses repeated `-`, trims
/// leading/trailing `-`, and caps at 32 bytes (same cap
/// `write_companion_preference_edges`'s inline `tag_safe` filter uses for style
/// tags). Returns `None` for input that normalizes to nothing (e.g. "" or "!!!").
///
/// Used both at NFT-ingestion time (turning free-text `nfts.genre`/`genres[]`
/// values into slugs before folding them into `tags`) and at genre-preference
/// write time (defensively re-sanitizing an already-slug frontend value before
/// it is embedded in a VID string).
pub(crate) fn sanitize_genre_slug(raw: &str) -> Option<String> {
    const MAX_SLUG_LEN: usize = 32;
    let mut slug = String::with_capacity(raw.len());
    let mut last_was_sep = false;
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_was_sep = false;
        } else if c.is_whitespace() || matches!(c, '_' | '/' | '-' | '&' | '+') {
            // Word-joining punctuation ("R&B", "Rock+Roll") becomes a separator
            // like whitespace, rather than being dropped and mashing the
            // surrounding words together ("r&b" -> "r-b", not "rb").
            if !last_was_sep && !slug.is_empty() {
                slug.push('-');
                last_was_sep = true;
            }
        }
        // Any other character (remaining punctuation, emoji, etc.) is dropped entirely.
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    slug.truncate(MAX_SLUG_LEN);
    // Truncation could leave a trailing '-' if it landed mid-separator.
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() { None } else { Some(slug) }
}

/// Build a post VID: `"post:{id}"`.
///
/// Idempotent: if `id` already carries the `"post:"` prefix (e.g. a VID returned
/// verbatim from a Nebula query via `parse_nebula_table`), the prefix is not
/// doubled. Without this guard the update cycle accumulates one extra `"post:"`
/// per hour — after ~18 cycles (UUID IDs) the VID overflows FIXED_STRING(128)
/// and storaged rejects it with "VID must fit space vertex id length limit".
///
/// Post IDs can be UUIDs (36 bytes) or Supabase UIDs (varied length); unlike
/// Ethereum addresses they are not fixed-width, so the overflow check is a
/// runtime tracing::error rather than a debug-only assertion — `debug_assert!`
/// compiles to nothing in release builds, allowing silent FIXED_STRING(128)
/// truncation in storaged if an oversized ID ever reaches this call site.
/// `is_safe_post_vid_id` already guards at the write-path boundary; this is a
/// belt-and-suspenders logging layer for unexpected future data shapes.
#[inline]
pub fn vid_post(id: &str) -> String {
    // Idempotency guard: strip an existing "post:" prefix so callers that feed
    // back Nebula-returned VIDs don't accumulate "post:post:post:..." over time.
    let bare = id.strip_prefix("post:").unwrap_or(id);
    if bare.len() + 5 > VID_MAX_LEN {
        tracing::error!(
            "post VID too long: 'post:{}' ({} bytes) exceeds FIXED_STRING(128) — \
             is_safe_post_vid_id should have rejected this; storaged will truncate",
            bare,
            bare.len() + 5,
        );
    }
    format!("post:{bare}")
}

// ── comments_on edge rank derivation ──────────────────────────────────────────
// NEBULA-002 / POOL-003 / TAG-S29-06: rank must derive deterministically from
// event_id so replaying the same event writes the same rank — INSERT EDGE at
// the same (src, dst, type, rank) tuple is idempotent, and Kafka retries don't
// create duplicate comment edges that corrupt comment_count aggregation in
// get_comment_fof_recommendations.
//
// graph_client.rs's write_comments_on and graph_sync.rs's inner_sync_comment
// each hand-derived this independently and drifted: the graph_client.rs copy
// took the raw first 16 hex digits after stripping only "0x" (breaks on UUID
// event_ids, which contain dashes that from_str_radix rejects — those always
// fall back to rank=0) and 16 hex digits overflow i64 for ~50% of hashes
// (any first nibble ≥ 8), silently collapsing distinct comments onto rank 0.
// This is the single corrected definition both call sites must share.

/// Derive a deterministic i64 rank for a `comments_on` edge from `event_id`.
///
/// Strips everything but hex digits (handles both tx-hash and UUID-style
/// event IDs), then takes the leading 15 hex digits — 60 bits, which always
/// fits in i64 (i64::MAX needs 63 bits, so 16 digits would overflow for
/// roughly half of all hash values). Falls back to 0 for empty/malformed input.
#[inline]
pub fn comment_rank(event_id: &str) -> i64 {
    let stripped: String = event_id
        .trim_start_matches("0x")
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();

    i64::from_str_radix(stripped.get(..15).unwrap_or("0"), 16).unwrap_or(0)
}

// ── Cross-system Redis key contract ──────────────────────────────────────────
// topic_affinity:{addr}:{tag} keys are written by the Elixir
// RecommendationSurfaceController and consumed by cache.rs::mget_topic_affinities.
// This constant is the canonical prefix — changing it here requires a matching
// change in the Elixir producer (search: @topic_affinity_prefix in that module).
pub const REDIS_TOPIC_AFFINITY_PREFIX: &str = "topic_affinity";

// ── Input validation ──────────────────────────────────────────────────────────
// A-02 / B-02: Promoted from private duplicates in graph_client.rs and
// graph_sync.rs so all Nebula-write paths share one definition.
// VID space is FIXED_STRING(64) (migration 25). "user:" prefix = 5 chars →
// address ≤ 59 chars (true need: 42, Ethereum-format-fixed). "post:" prefix
// = 5 chars → post_id ≤ 59 chars (true need: 36, uuid-format-fixed).
// These functions are the authoritative VID-safety contract for the whole crate.

/// Validate a user address for safe embedding in a Nebula VID or property string.
/// Requires "0x" prefix, exactly 42 chars, lowercase hex only.
/// EIP-55 checksummed addresses (mixed-case) must be normalised with
/// `normalize_address` before calling this function.
#[inline]
pub(crate) fn is_safe_address(addr: &str) -> bool {
    addr.starts_with("0x")
        && addr.len() == 42
        && addr[2..].chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Validate a generic event/tx ID for safe embedding in a Nebula property string.
#[inline]
pub(crate) fn is_safe_id(id: &str) -> bool {
    id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
}

/// Validate a post/NFT UUID for safe embedding in a Nebula VID.
/// VID space is FIXED_STRING(64) (migration 25). "post:" prefix = 5 chars →
/// post_id ≤ 59 chars.
///
/// Explicitly rejects already-prefixed IDs (those starting with `"post:"` or
/// `"user:"`) so that `vid_post()` callers receiving raw Nebula VIDs cannot
/// silently produce `"post:post:…"` double-prefix strings that accumulate each
/// update cycle. The idempotency guard in `vid_post` is the primary fix; this
/// validator is a belt-and-suspenders layer so such IDs are skipped entirely at
/// the write-path boundary rather than being silently corrected.
#[inline]
pub(crate) fn is_safe_post_vid_id(id: &str) -> bool {
    !id.contains(':')
        && id.len() <= 59
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ── Nebula vertex upsert nGQL helpers ─────────────────────────────────────────
// A-04: 8 inner_sync_* methods in graph_sync.rs and 6 write_* methods in
// graph_client.rs all begin with the same INSERT VERTEX IF NOT EXISTS prefix.
// Centralise here so a schema change (new required property) touches one place.

/// nGQL fragment that upserts a `user` vertex without overwriting existing data.
/// Returns a single-statement string (no trailing newline) ready for concatenation
/// into a multi-statement nGQL batch.
pub fn ensure_user_vertex_nql(vid: &str, addr: &str) -> String {
    format!(
        "INSERT VERTEX IF NOT EXISTS user(id, username, followers_count, \
         following_count, total_likes_given, total_posts) \
         VALUES \"{vid}\":(\"{addr}\", \"\", 0, 0, 0, 0);"
    )
}

/// nGQL fragment that upserts a `post` vertex without overwriting existing data.
pub fn ensure_post_vertex_nql(vid: &str, id: &str) -> String {
    format!(
        "INSERT VERTEX IF NOT EXISTS post(id, content, author_id, views, \
         likes, hashtags, content_type) \
         VALUES \"{vid}\":(\"{id}\", \"\", \"\", 0, 0, \"\", \"\");"
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────


#[cfg(test)]
mod tests;
