//! Recommendation Module
//!
//! Provides personalized NFT recommendations for TheraGraph users.
//!
//! ## Architecture
//!
//! 1. **Preferences** - Track user behavior (likes, purchases, views) to build preference profiles
//! 2. **Features** - Extract semantic features from NFT metadata (tags, style, mood, genre)
//! 3. **Engine** - Score and rank NFTs based on user preferences and content features
//!
//! ## Feed Types
//!
//! - **Following Feed**: NFTs from creators the user follows (chronological with engagement boost)
//! - **Enhanced Feed**: Personalized recommendations from all creators (smart algorithm)
//!
//! ## Algorithm Overview
//!
//! The enhanced feed uses a multi-factor scoring system:
//! - Tag matching (30%): NFTs with tags user has engaged with
//! - Content type affinity (15%): Preference for snap/art/music/flix
//! - Creator affinity (15%): Creators user has engaged with before
//! - Trending score (10%): Currently popular content
//! - Engagement score (10%): Overall engagement level
//! - Quality score (10%): Creator reputation
//! - Recency (5%): Newer content bonus
//! - Diversity penalty (5%): Avoid too much from same creator/tags

pub mod cache;
pub mod candidate_repository;
pub mod coalescer;
pub mod engine;
pub mod recommended_to_buffer;
pub mod features;
pub mod graph_client;
pub mod graph_dlq;
pub(crate) mod graph_transport;
pub mod model;
pub mod preferences;
pub mod recorder;
pub mod schema_consts;
pub mod scoring;
pub mod types;
pub mod updater;
pub mod metrics;
pub mod weights;
pub mod feedsource_impls;

// Re-export the types that are actually used externally
pub use engine::ScoredNft;
pub use preferences::UserPreferences;
// Metrics are used internally by the engine

/// Uniform interface for every feed source in the recommendation pipeline.
///
/// Each fallback tier (personalized → SQL cache → trending → following) is an
/// adapter implementing this trait. The API router dispatches through
/// `Arc<dyn FeedSource>` so adding a new feed type requires only a new struct
/// plus `impl FeedSource`, with zero changes to the handler or the engine.
///
/// C6: trait is now live — `feedsource_impls.rs` contains four active adapters
/// wired into the API handlers via `AppState.feeds`.
///
/// # Contract
/// - `candidates` MUST be pure with respect to external state (no side effects)
/// - Empty `Vec` signals "this source has no useful results right now"
/// - `ScoredNft.score` MUST be in [0.0, 1.0] — clamp before returning
///
/// Adding a new fallback = new struct + `impl FeedSource`, zero changes to engine.
#[async_trait::async_trait]
pub trait FeedSource: Send + Sync {
    /// Unique human-readable name for logging/metrics (e.g. `"personalized"`, `"trending"`).
    fn name(&self) -> &'static str;

    /// Return scored candidates for `user_address`, up to `limit` items.
    /// Filter to `contract_type` if `Some`.
    ///
    /// Offset is intentionally absent — FeedSource is a single-page interface.
    /// The cache layer (get_enhanced_feed_cached / get_recommendations_coalesced)
    /// owns pagination; adapters always request from position 0.
    async fn candidates(
        &self,
        user_address: &str,
        limit: usize,
        contract_type: Option<&str>,
    ) -> anyhow::Result<Vec<ScoredNft>>;
}
