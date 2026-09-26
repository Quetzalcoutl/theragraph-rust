//! Application state shared across all top-level services.
//!
//! Extracted from `main.rs` so the struct and its dependencies can be
//! imported by sub-modules (`event_processor`, `indexer`) without
//! reaching into the binary entry point.

use std::sync::Arc;
use tokio::sync::broadcast;

use crate::{
    config::Config,
    database::Database,
    event_processor::DirectHandlers,
    kafka::KafkaProducer,
    recommendation,
};

/// Shared application state passed to every service via `Arc<AppState>`.
pub struct AppState {
    pub config: Arc<Config>,
    /// Recommendation engine pool — large, throughput-optimised.
    pub db: Database,
    /// Dedicated small pool for the blockchain indexer.
    /// Keeps cursor writes latency-bound regardless of recommendation update bursts.
    /// Size controlled by INDEXER_POOL_SIZE (default 5).
    pub indexer_db: Database,
    pub elixir_db: Database,
    pub kafka: KafkaProducer,
    pub shutdown: broadcast::Sender<()>,
    pub rec_cache: Option<recommendation::cache::RecCache>,
    pub graph_client: Arc<dyn recommendation::graph_client::GraphTraversal>,
    /// Set when KAFKA_ENABLED=false so indexers can write preference signals directly.
    pub direct_handlers: Option<Arc<DirectHandlers>>,
    /// RS-03: TaskTracker for fire-and-forget graph write tasks.
    pub task_tracker: Arc<tokio_util::task::TaskTracker>,
}
