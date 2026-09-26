//! Re-export shim — keeps existing `super::features::X` call paths working.
//!
//! Pure logic lives in `feature_extractor`; Postgres persistence in `feature_store`.
pub use super::feature_extractor::{extract_features, NftFeatures, ScoringFeatures};
pub use super::feature_store::{
    get_features_batch, save_features, update_engagement_scores, update_trending_scores,
};
