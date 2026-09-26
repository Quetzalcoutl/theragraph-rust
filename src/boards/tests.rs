    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn fresh_entry_not_expired() {
        let entry = CacheEntry {
            data: 42u32,
            inserted_at: Instant::now(),
            ttl: Duration::from_secs(60),
        };
        assert!(!entry.is_expired());
    }

    #[test]
    fn expired_entry_is_expired() {
        let entry = CacheEntry {
            data: 42u32,
            inserted_at: Instant::now() - Duration::from_secs(120),
            ttl: Duration::from_secs(60),
        };
        assert!(entry.is_expired());
    }

    #[test]
    fn zero_ttl_always_expired() {
        let entry = CacheEntry {
            data: 42u32,
            inserted_at: Instant::now() - Duration::from_millis(1),
            ttl: Duration::ZERO,
        };
        assert!(entry.is_expired());
    }

    #[test]
    fn boundary_ttl_equal_elapsed_not_expired() {
        // is_expired uses `>` not `>=`, so an entry whose elapsed is at or
        // below the TTL must NOT be considered expired.
        let entry = CacheEntry {
            data: 42u32,
            inserted_at: Instant::now(),
            ttl: Duration::from_secs(60),
        };
        assert!(!entry.is_expired());
    }
