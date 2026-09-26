    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::recommendation::scoring::RecommendationReason;

    fn nft(score: f32) -> ScoredNft {
        ScoredNft {
            nft_id: "test".into(),
            token_id: 0,
            contract_address: "0x0".into(),
            score,
            reason: RecommendationReason::Discovery,
            contract_type: "snap".into(),
            creator_address: "0x0".into(),
            tags: vec![].into(),
        }
    }

    fn nfts(count: usize) -> Vec<ScoredNft> {
        (0..count).map(|i| nft(i as f32 / 100.0)).collect()
    }

    fn read_fn(
        data: Vec<ScoredNft>,
    ) -> impl Fn() -> BoxFuture<'static, Result<Option<Vec<ScoredNft>>>> + Send {
        move || {
            let d = data.clone();
            Box::pin(async move { Ok(Some(d)) })
        }
    }

    fn cold_read(
    ) -> impl Fn() -> BoxFuture<'static, Result<Option<Vec<ScoredNft>>>> + Send {
        || Box::pin(async { Ok(None) })
    }

    /// Jon Gjengset / Dan Lickly:
    /// Cache with exactly `offset` items must trigger a miss, not a hit.
    /// `min_cached = offset + 1` is the production invariant — fixes permanent-miss loop.
    #[tokio::test]
    async fn cache_miss_when_exactly_offset_items() {
        let c = StampedeCoalescer::new(2);
        let offset: usize = 19;
        let limit: usize = 20;
        let misses = Arc::new(AtomicUsize::new(0));
        let hits = Arc::new(AtomicUsize::new(0));
        let miss_c = misses.clone();
        let hit_c = hits.clone();

        let _result = c
            .run(
                "key1".into(),
                offset + 1, // min_cached — exactly one more than offset
                offset,     // slice_skip
                limit,      // slice_take
                move || { hit_c.fetch_add(1, Ordering::Relaxed); },
                move || { miss_c.fetch_add(1, Ordering::Relaxed); },
                read_fn(nfts(offset)), // cache has exactly offset items — one short
                || async { Ok(nfts(limit)) },
                |_| Box::pin(async {}),
            )
            .await
            .unwrap();

        assert_eq!(
            misses.load(Ordering::Relaxed), 1,
            "cache with exactly offset items must be a miss (offset={offset}, min_cached={})",
            offset + 1
        );
        assert_eq!(hits.load(Ordering::Relaxed), 0);
    }

    /// Cache with `offset + 1` items is a hit; slice returns items[offset..].
    #[tokio::test]
    async fn cache_hit_when_min_cached_met() {
        let c = StampedeCoalescer::new(2);
        let offset: usize = 5;
        let limit: usize = 10;
        let hits = Arc::new(AtomicUsize::new(0));
        let hit_c = hits.clone();

        let result = c
            .run(
                "key2".into(),
                offset + 1, // min_cached = 6
                offset,     // slice_skip = 5
                limit,      // slice_take = 10
                move || { hit_c.fetch_add(1, Ordering::Relaxed); },
                || {},
                read_fn(nfts(offset + 1)), // 6 items — exactly min_cached
                || async { panic!("compute must not run on a cache hit") },
                |_| Box::pin(async {}),
            )
            .await
            .unwrap();

        assert_eq!(hits.load(Ordering::Relaxed), 1);
        // skip 5 from 6 items → 1 item returned
        assert_eq!(result.len(), 1, "slice(skip={offset}, take={limit}) of 6 items → 1");
    }

    /// Non-paginated path: min_cached = limit, slice_skip = 0.
    #[tokio::test]
    async fn non_paginated_full_cache_hit() {
        let c = StampedeCoalescer::new(2);
        let limit = 20;
        let hits = Arc::new(AtomicUsize::new(0));
        let hit_c = hits.clone();

        let result = c
            .run(
                "key3".into(),
                limit, 0, limit,
                move || { hit_c.fetch_add(1, Ordering::Relaxed); },
                || {},
                read_fn(nfts(limit)),
                || async { panic!("compute must not run") },
                |_| Box::pin(async {}),
            )
            .await
            .unwrap();

        assert_eq!(hits.load(Ordering::Relaxed), 1);
        assert_eq!(result.len(), limit);
    }

    /// Cold cache triggers compute; write_cache is called with result.
    #[tokio::test]
    async fn cold_cache_triggers_compute_and_write() {
        let c = StampedeCoalescer::new(2);
        let written = Arc::new(AtomicUsize::new(0));
        let written_c = written.clone();

        let result = c
            .run(
                "key4".into(),
                10, 0, 10,
                || {},
                || {},
                cold_read(),
                || async { Ok(nfts(10)) },
                move |items| {
                    written_c.fetch_add(items.len(), Ordering::Relaxed);
                    Box::pin(async {})
                },
            )
            .await
            .unwrap();

        assert_eq!(result.len(), 10);
        assert_eq!(
            written.load(Ordering::Relaxed), 10,
            "write_cache must fire after compute with the computed items"
        );
    }
