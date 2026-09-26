use super::*;
    use super::*;
    use anyhow::Result as AnyhowResult;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── Shared-state mock transport ───────────────────────────────────────────

    /// Pre-queued responses with an externally observable call counter.
    ///
    /// `Arc` internals allow the test to inspect counts after `GraphSync` takes
    /// ownership of the transport.
    #[derive(Clone)]
    pub(super) struct MockTransport {
        responses: Arc<Mutex<VecDeque<AnyhowResult<String>>>>,
        call_count: Arc<AtomicUsize>,
    }

    impl MockTransport {
        /// Queue `ok_count` successes.
        pub(super) fn always_ok(ok_count: usize) -> Self {
            Self {
                responses: Arc::new(Mutex::new(
                    (0..ok_count).map(|_| Ok(String::new())).collect(),
                )),
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// Queue `fail_count` "connection refused" errors followed by one success.
        pub(super) fn fail_then_ok(fail_count: usize) -> Self {
            let mut q: VecDeque<AnyhowResult<String>> = (0..fail_count)
                .map(|_| Err(anyhow::anyhow!("connection refused (mock)")))
                .collect();
            q.push_back(Ok(String::new()));
            Self {
                responses: Arc::new(Mutex::new(q)),
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        pub(super) fn call_count(&self) -> usize {
            self.call_count.load(Ordering::Relaxed)
        }
    }

    impl GraphTransport for MockTransport {
        fn execute(&self, _query: &str) -> impl std::future::Future<Output = AnyhowResult<String>> + Send {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockTransport: no more queued responses — add more to the queue");
            async move { response }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    const FOLLOWER: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TARGET:   &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const TX:       &str = "abc123tx";

    fn make_sync(transport: MockTransport) -> GraphSync<MockTransport> {
        GraphSync::with_client(GraphClient::with_transport(transport))
    }

    // ── Test 1: succeeds on first try — single transport call ─────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn sync_follow_succeeds_on_first_try() {
        let transport = MockTransport::always_ok(1);
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        let result = sync.sync_follow(FOLLOWER, TARGET, TX).await;

        assert!(result.is_ok(), "expected Ok on first try, got: {:?}", result);
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            1,
            "expected exactly 1 transport call on first-try success"
        );
    }

    // ── Test 2: follows are retried — inner_sync_follow propagates Nebula errors ─

    #[tokio::test(flavor = "current_thread")]
    async fn sync_follow_retries_transient_errors() {
        let transport = MockTransport::fail_then_ok(2);
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        let result = sync.sync_follow(FOLLOWER, TARGET, TX).await;

        assert!(result.is_ok(), "expected Ok after retries succeeded: {:?}", result);
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            3,
            "sync_follow should retry transient failures (expected 3 transport calls)"
        );
    }

    // ── Test 3: invalid input is rejected immediately, no transport calls ─────

    #[tokio::test(flavor = "current_thread")]
    async fn sync_follow_invalid_input_not_retried() {
        let transport = MockTransport::always_ok(10);
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        let result = sync.sync_follow("not-an-address", TARGET, TX).await;

        assert!(
            matches!(result, Err(GraphSyncError::InvalidInput { .. })),
            "expected InvalidInput error, got: {:?}",
            result
        );
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            0,
            "transport must not be called for InvalidInput"
        );
    }

    // ── Test 4: is_transient classification ──────────────────────────────────

    #[test]
    fn connection_refused_is_transient() {
        let e = GraphSyncError::ConnectionError {
            operation: "test",
            source: anyhow::anyhow!("connection refused"),
        };
        assert!(e.is_transient());
    }

    #[test]
    fn timeout_is_transient() {
        let e = GraphSyncError::ConnectionError {
            operation: "test",
            source: anyhow::anyhow!("nebula-console query timed out (30s)"),
        };
        assert!(e.is_transient());
    }

    #[test]
    fn circuit_open_is_transient() {
        let e = GraphSyncError::ConnectionError {
            operation: "test",
            source: anyhow::anyhow!("Nebula circuit open — consecutive failures exceeded threshold"),
        };
        assert!(e.is_transient());
    }

    #[test]
    fn invalid_input_is_not_transient() {
        let e = GraphSyncError::InvalidInput {
            operation: "test",
            detail: "bad address".to_string(),
        };
        assert!(!e.is_transient());
    }

    // ── content_ops: sync_content_minted / sync_listing_updated (migration 26) ─

    // Postgres-UUID-shaped, matching the real VID-FIX convention (nfts.id, not
    // the raw on-chain token id) every graph_sync caller actually passes.
    const POST_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[tokio::test(flavor = "current_thread")]
    async fn sync_content_minted_succeeds_on_first_try() {
        let transport = MockTransport::always_ok(1);
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        let result = sync.sync_content_minted(POST_UUID, 2.5).await;

        assert!(result.is_ok(), "expected Ok on first try, got: {:?}", result);
        assert_eq!(call_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_content_minted_invalid_token_id_not_retried() {
        let transport = MockTransport::always_ok(10);
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        // ':' is rejected by is_safe_post_vid_id (VID separator character).
        let result = sync.sync_content_minted("post:550e8400", 2.5).await;

        assert!(matches!(result, Err(GraphSyncError::InvalidInput { .. })));
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            0,
            "transport must not be called for InvalidInput"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_listing_updated_succeeds_on_first_try() {
        let transport = MockTransport::always_ok(1);
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        let result = sync.sync_listing_updated(POST_UUID, 5.0, 100).await;

        assert!(result.is_ok(), "expected Ok on first try, got: {:?}", result);
        assert_eq!(call_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_listing_updated_retries_transient_errors() {
        let transport = MockTransport::fail_then_ok(2);
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        let result = sync.sync_listing_updated(POST_UUID, 5.0, 0).await;

        assert!(result.is_ok(), "expected Ok after retries succeeded: {:?}", result);
        assert_eq!(call_count.load(Ordering::Relaxed), 3);
    }
