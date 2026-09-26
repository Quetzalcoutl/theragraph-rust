//! Redis Cache Layer for Recommendation Engine
//!
//! Provides cache-aside pattern for Nebula graph queries, NFT features,
//! user preferences, and recommendation results.
//!
//! Joint optimization by Parity Technologies & Ferrous Systems:
//! - ConnectionManager for automatic reconnection
//! - Binary serialization with serde_json for complex types
//! - TTL-based expiration with configurable durations
//! - Graceful degradation: cache misses fall through to DB/Nebula

pub mod keys;
mod ops;
mod nft;
mod user;
mod social;
mod signal;

pub use keys::*;
pub use ops::RecCache;

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests;
