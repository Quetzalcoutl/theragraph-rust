use super::*;

    /// Regression test for the prompt-detection hang: requires a real
    /// nebula-console binary on PATH and a live graphd (defaults to
    /// 127.0.0.1:9669, matching local docker-compose). Not run by default —
    /// `cargo test -- --ignored nebula_pool_transport_connects_and_queries`.
    /// A pure-unit test cannot catch this class of bug: the hang only occurs
    /// against a real subprocess's raw stdout bytes, not any mock transport.
    #[tokio::test]
    #[ignore]
    async fn nebula_pool_transport_connects_and_queries() {
        unsafe {
            std::env::set_var("NEBULA_HOST", "127.0.0.1");
            std::env::set_var("NEBULA_POOL_SIZE", "2");
        }
        let pool = NebulaPoolTransport::from_env()
            .await
            .expect("pool should connect to live Nebula");
        let output = pool.execute("SHOW HOSTS;").await.expect("query should succeed");
        assert!(output.contains("ONLINE"), "expected SHOW HOSTS output, got: {output}");
    }
