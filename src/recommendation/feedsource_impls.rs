//! Concrete `FeedSource` adapters wired into the API router.
//!
//! Each adapter wraps `Arc<RecommendationEngine>` and implements the
//! `FeedSource` trait. The API router dispatches through `dyn FeedSource`
//! so adding a new feed type (CuratorFeed, DiscoveryFeed, …) requires only
//! implementing this trait — zero changes to the handler or the engine.
//!
//! ## Adapter → engine method map (Jobs audit: names must match what they do)
//!
//! | Adapter           | Engine method              | Route                   |
//! |-------------------|----------------------------|-------------------------|
//! | `FollowingFeed`   | `get_following_feed`       | `/api/v1/feed/:addr`    |
//! | `EnhancedFeed`    | `get_enhanced_feed`        | `/api/v1/enhanced-feed` |
//! | `PersonalizedFeed`| `get_recommendations`      | (engine direct)         |
//! | `TrendingFeed`    | `get_enhanced_feed(0x0)`   | `/api/v1/trending`      |
use crate::recommendation::engine::{RecommendationEngine, ScoredNft};
use crate::recommendation::FeedSource;
use anyhow::Result;
use std::sync::Arc;

use super::schema_consts::{
    FEED_TYPE_ENHANCED, FEED_TYPE_FOLLOWING, FEED_TYPE_PERSONALIZED,
    FEED_TYPE_TRENDING,
};

pub struct PersonalizedFeed(pub Arc<RecommendationEngine>);

impl PersonalizedFeed {
    /// Returns the feed name as a compile-time constant — useful for tests
    /// that cannot construct an engine (requires a live `PgPool`).
    pub const fn static_name() -> &'static str {
        FEED_TYPE_PERSONALIZED
    }
}

#[async_trait::async_trait]
impl FeedSource for PersonalizedFeed {
    fn name(&self) -> &'static str {
        Self::static_name()
    }

    async fn candidates(
        &self,
        user_address: &str,
        limit: usize,
        contract_type: Option<&str>,
    ) -> Result<Vec<ScoredNft>> {
        self.0
            .get_recommendations(user_address, limit, contract_type, false)
            .await
    }
}

/// Serves the enhanced/personalized algorithm for a specific user.
///
/// Jobs audit: previously named `TrendingFeed` despite calling `get_enhanced_feed`.
/// Renamed to `EnhancedFeed` so the name matches the method.
pub struct EnhancedFeed(pub Arc<RecommendationEngine>);

impl EnhancedFeed {
    pub const fn static_name() -> &'static str { FEED_TYPE_ENHANCED }
}

#[async_trait::async_trait]
impl FeedSource for EnhancedFeed {
    fn name(&self) -> &'static str { Self::static_name() }
    async fn candidates(&self, user_address: &str, limit: usize, contract_type: Option<&str>) -> Result<Vec<ScoredNft>> {
        // FeedSource has no offset concept — always serve from position 0 (cache-first).
        // Callers that need pagination should use get_enhanced_feed_cached directly.
        self.0.get_enhanced_feed_cached(user_address, limit, 0, contract_type).await
    }
}

/// Serves content trending across all users (popularity-based, no personalisation).
///
/// Uses `get_enhanced_feed` with the zero address so there are no user-specific
/// signals — the algorithm falls back to engagement/quality/recency ranking.
pub struct TrendingFeed(pub Arc<RecommendationEngine>);

impl TrendingFeed {
    /// Returns the feed name as a compile-time constant — useful for tests
    /// that cannot construct an engine (requires a live `PgPool`).
    pub const fn static_name() -> &'static str {
        FEED_TYPE_TRENDING
    }
    /// Zero address — no user signals, produces pure popularity-based ranking.
    const TRENDING_USER: &'static str = "0x0000000000000000000000000000000000000000";
}

#[async_trait::async_trait]
impl FeedSource for TrendingFeed {
    fn name(&self) -> &'static str {
        Self::static_name()
    }

    async fn candidates(
        &self,
        _user_address: &str,
        limit: usize,
        contract_type: Option<&str>,
    ) -> Result<Vec<ScoredNft>> {
        // Ignore user_address: trending is always population-wide.
        // Cache-first via get_enhanced_feed_cached so trending feed warms once
        // and all concurrent callers share the same compute result.
        self.0
            .get_enhanced_feed_cached(Self::TRENDING_USER, limit, 0, contract_type)
            .await
    }
}

pub struct FollowingFeed(pub Arc<RecommendationEngine>);

impl FollowingFeed {
    /// Returns the feed name as a compile-time constant — useful for tests
    /// that cannot construct an engine (requires a live `PgPool`).
    pub const fn static_name() -> &'static str {
        FEED_TYPE_FOLLOWING
    }
}

#[async_trait::async_trait]
impl FeedSource for FollowingFeed {
    fn name(&self) -> &'static str {
        Self::static_name()
    }

    async fn candidates(
        &self,
        user_address: &str,
        limit: usize,
        _contract_type: Option<&str>,
    ) -> Result<Vec<ScoredNft>> {
        self.0.get_following_feed(user_address, limit, 0).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each feed struct wraps `Arc<RecommendationEngine>`, which requires a live
    // `PgPool` to construct — not available in unit tests.  We expose
    // `static_name()` on each struct so the string constants can be verified
    // without touching any I/O.

    #[test]
    fn personalized_feed_static_name() {
        assert_eq!(PersonalizedFeed::static_name(), "personalized");
    }

    #[test]
    fn trending_feed_static_name() {
        assert_eq!(TrendingFeed::static_name(), "trending");
    }

    #[test]
    fn following_feed_static_name() {
        assert_eq!(FollowingFeed::static_name(), "following");
    }

    /// Smoke-test that every feed name constant is non-empty and unique.
    #[test]
    fn feed_names_are_distinct() {
        let names = [
            PersonalizedFeed::static_name(),
            TrendingFeed::static_name(),
            FollowingFeed::static_name(),
        ];
        for name in &names {
            assert!(!name.is_empty(), "feed name must not be empty");
        }
        let unique: std::collections::HashSet<_> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "feed names must be unique");
    }
}
