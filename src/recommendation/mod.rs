//! Recommendation Module
//!
//! Provides personalized NFT recommendations for TheraGraph users.
//!
//! ## Architecture
//!
//! 1. **Preferences** - Track user behavior (likes, purchases, views) to build preference profiles
//! 2. **Features** - Extract semantic features from NFT metadata (tags, style, mood, genre)
//! 3. **Engine** - Score and rank NFTs based on user preferences and content features
//!
//! ## Feed Types
//!
//! - **Following Feed**: NFTs from creators the user follows (chronological with engagement boost)
//! - **Enhanced Feed**: Personalized recommendations from all creators (smart algorithm)
//!
//! ## Algorithm Overview
//!
//! The enhanced feed uses a multi-factor scoring system:
//! - Tag matching (30%): NFTs with tags user has engaged with
//! - Content type affinity (15%): Preference for snap/art/music/flix
//! - Creator affinity (15%): Creators user has engaged with before
//! - Trending score (10%): Currently popular content
//! - Engagement score (10%): Overall engagement level
//! - Quality score (10%): Creator reputation
//! - Recency (5%): Newer content bonus
//! - Diversity penalty (5%): Avoid too much from same creator/tags

pub mod engine;
pub mod features;
pub mod graph_client;
pub mod preferences;
pub mod updater;

// Re-export the types that are actually used externally
pub use engine::ScoredNft;
pub use preferences::UserPreferences;
