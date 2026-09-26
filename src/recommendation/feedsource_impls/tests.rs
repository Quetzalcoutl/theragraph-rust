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
