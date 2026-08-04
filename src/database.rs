//! Database connection pool and utilities
//!
//! Provides a robust PostgreSQL connection pool with:
//! - Configurable pool sizes and timeouts
//! - Health checking
//! - Query instrumentation
//! - Connection lifecycle management

use crate::config::DatabaseConfig;
use crate::error::{Error, Result};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::ConnectOptions;
use std::str::FromStr;
use std::time::Duration;
use tracing::{debug, info, instrument};

/// Database connection pool
#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// Create a new database connection pool
    #[instrument(skip(config))]
    pub async fn new(config: &DatabaseConfig) -> Result<Self> {
        let pool = create_pool(config).await?;
        Ok(Self { pool })
    }

    /// Get reference to the underlying pool
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Check if database is healthy
    pub async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| Error::Database {
                message: format!("Health check failed: {}", e).into(),
                source: Some(e),
            })?;
        Ok(())
    }

    /// Get pool statistics
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            size: self.pool.size(),
            idle: self.pool.num_idle(),
        }
    }

    /// Close all connections gracefully
    pub async fn close(&self) {
        info!("Closing database connection pool...");
        self.pool.close().await;
        info!("Database connection pool closed");
    }
}

/// Pool statistics
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub size: u32,
    pub idle: usize,
}

/// Create a connection pool with the given configuration
pub async fn create_pool(config: &DatabaseConfig) -> Result<PgPool> {
    info!("Creating database connection pool...");
    debug!(
        "Pool config: max={}, min={}, connect_timeout={:?}",
        config.max_connections, config.min_connections, config.connect_timeout
    );

    // Parse connection options
    let mut connect_options =
        PgConnectOptions::from_str(&config.url).map_err(|e| Error::Config {
            message: format!("Invalid database URL: {}", e).into(),
            source: None,
        })?;

    // Set statement cache
    connect_options = connect_options.statement_cache_capacity(config.statement_cache_size);

    // Disable logging of every query in production (can be enabled via SQLX_LOG=true)
    connect_options = connect_options.log_statements(log::LevelFilter::Debug);
    connect_options =
        connect_options.log_slow_statements(log::LevelFilter::Warn, Duration::from_secs(1));

    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .acquire_timeout(config.connect_timeout)
        .idle_timeout(Some(config.idle_timeout))
        .max_lifetime(Some(config.max_lifetime))
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                let timeout_ms: u32 = std::env::var("DB_STATEMENT_TIMEOUT_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(8_000);
                assert!(timeout_ms <= 3_600_000, "implausible DB_STATEMENT_TIMEOUT_MS: {timeout_ms}");
                sqlx::query(&format!("SET statement_timeout = {timeout_ms}"))
                    .execute(&mut *conn)
                    .await?;
                debug!("New database connection established (statement_timeout={}ms)", timeout_ms);
                Ok(())
            })
        })
        .before_acquire(|_conn, _meta| {
            Box::pin(async move {
                // Could add connection validation here
                Ok(true)
            })
        })
        .after_release(|_conn, _meta| {
            Box::pin(async move {
                // Could add cleanup here
                Ok(true)
            })
        })
        .connect_with(connect_options)
        .await
        .map_err(|e| Error::Database {
            message: format!("Failed to create connection pool: {}", e).into(),
            source: Some(e),
        })?;

    // Verify we can connect
    sqlx::query("SELECT 1")
        .fetch_one(&pool)
        .await
        .map_err(|e| Error::Database {
            message: format!("Failed to verify database connection: {}", e).into(),
            source: Some(e),
        })?;

    info!(
        "Database connection pool created (size: {}, idle: {})",
        pool.size(),
        pool.num_idle()
    );

    Ok(pool)
}

/// Run database migrations
#[instrument(skip(pool))]
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    info!("Running database migrations...");

    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| Error::Migration(e.to_string()))?;

    info!("Database migrations completed successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
