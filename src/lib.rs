//! TheraGraph library crate
//!
//! Re-exports core modules for integration tests and external use.

pub mod recommendation;
pub mod kafka;
pub mod config;
pub mod database;
pub mod error;

// Re-export commonly used types
pub use recommendation::*;
pub use kafka::KafkaProducer;
pub use config::Config;
pub use database::Database;
pub use error::Result;
