    use super::*;

    #[tokio::test]
    async fn test_pool_stats() {
        // This test requires a running database
        // Skip in CI without database
        if std::env::var("DATABASE_URL").is_err() {
            return;
        }

        let config = DatabaseConfig {
            url: std::env::var("DATABASE_URL").unwrap(),
            max_connections: 5,
            min_connections: 1,
            connect_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(60),
            max_lifetime: Duration::from_secs(300),
            statement_cache_size: 10,
        };

        let db = Database::new(&config).await.unwrap();
        let stats = db.stats();

        assert!(stats.size > 0);
        db.close().await;
    }
