//! Recommendation Metrics and Performance Monitoring
//!
//! ByteGraph-inspired metrics tracking for recommendation quality and performance.
//! These utilities are used for monitoring and debugging recommendation quality.

#![allow(dead_code)]  // Metrics are used selectively during profiling

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

/// Metrics for a single recommendation request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendationMetrics {
    pub user_address: String,
    pub request_id: String,
    pub timestamp: i64,
    
    // Performance metrics
    pub total_duration_ms: u64,
    pub candidate_fetch_ms: u64,
    pub scoring_duration_ms: u64,
    pub feature_fetch_ms: u64,
    
    // Quality metrics
    pub candidates_considered: usize,
    pub recommendations_returned: usize,
    pub avg_score: f32,
    pub score_distribution: HashMap<String, usize>,  // reason -> count
    
    // Diversity metrics
    pub unique_creators: usize,
    pub unique_tags: usize,
    pub content_type_distribution: HashMap<String, usize>,
    
    // ByteGraph metrics
    pub tag_match_count: usize,
    pub creator_match_count: usize,
    pub content_type_match_count: usize,
    pub discovery_count: usize,
}

impl Default for RecommendationMetrics {
    fn default() -> Self {
        Self {
            user_address: String::new(),
            request_id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            total_duration_ms: 0,
            candidate_fetch_ms: 0,
            scoring_duration_ms: 0,
            feature_fetch_ms: 0,
            candidates_considered: 0,
            recommendations_returned: 0,
            avg_score: 0.0,
            score_distribution: HashMap::new(),
            unique_creators: 0,
            unique_tags: 0,
            content_type_distribution: HashMap::new(),
            tag_match_count: 0,
            creator_match_count: 0,
            content_type_match_count: 0,
            discovery_count: 0,
        }
    }
}

/// Performance timer for tracking operation duration
pub struct PerformanceTimer {
    start: Instant,
    label: String,
}

impl PerformanceTimer {
    pub fn new(label: &str) -> Self {
        Self {
            start: Instant::now(),
            label: label.to_string(),
        }
    }
    
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    
    pub fn log_if_slow(&self, threshold_ms: u64) {
        let elapsed = self.elapsed_ms();
        if elapsed > threshold_ms {
            tracing::warn!(
                "⚠️ Slow operation: {} took {}ms (threshold: {}ms)",
                self.label,
                elapsed,
                threshold_ms
            );
        }
    }
}

impl Drop for PerformanceTimer {
    fn drop(&mut self) {
        let elapsed = self.elapsed_ms();
        tracing::debug!("⏱️ {} completed in {}ms", self.label, elapsed);
    }
}

/// Recommendation quality analyzer
pub struct QualityAnalyzer;

impl QualityAnalyzer {
    /// Calculate diversity score (0-1, higher is better)
    /// Based on ByteGraph principle: recommendations should have variety
    pub fn diversity_score(
        unique_creators: usize,
        unique_tags: usize,
        total_recommendations: usize,
    ) -> f32 {
        if total_recommendations == 0 {
            return 0.0;
        }
        
        let creator_diversity = unique_creators as f32 / total_recommendations as f32;
        let tag_diversity_raw = (unique_tags as f32 / (total_recommendations as f32 * 3.0)).min(1.0);
        // Scale tag diversity by creator diversity so tags contribute only when creator diversity is meaningful
        let tag_diversity = tag_diversity_raw * creator_diversity;

        // Weighted average: creators matter more than tags
        creator_diversity * 0.7 + tag_diversity * 0.3
    }
    
    /// Calculate personalization score (0-1, higher is better)
    /// Ratio of recommendations matching user preferences
    pub fn personalization_score(
        tag_matches: usize,
        creator_matches: usize,
        content_type_matches: usize,
        total_recommendations: usize,
    ) -> f32 {
        if total_recommendations == 0 {
            return 0.0;
        }
        
        let total_matches = tag_matches + creator_matches + content_type_matches;
        (total_matches as f32 / total_recommendations as f32).min(1.0)
    }
    
    /// Detect potential issues with recommendation quality
    pub fn detect_issues(metrics: &RecommendationMetrics) -> Vec<String> {
        let mut issues = Vec::new();
        
        // Low diversity
        let diversity = Self::diversity_score(
            metrics.unique_creators,
            metrics.unique_tags,
            metrics.recommendations_returned,
        );
        if diversity < 0.3 {
            issues.push(format!("Low diversity: {:.2}", diversity));
        }
        
        // Too slow
        if metrics.total_duration_ms > 200 {
            issues.push(format!("Slow response: {}ms", metrics.total_duration_ms));
        }
        
        // Too many discovery recommendations (lack of personalization)
        let discovery_ratio = metrics.discovery_count as f32 
            / metrics.recommendations_returned.max(1) as f32;
        if discovery_ratio > 0.5 {
            issues.push(format!("High discovery ratio: {:.2}%", discovery_ratio * 100.0));
        }
        
        // Low average score
        if metrics.avg_score < 0.3 {
            issues.push(format!("Low avg score: {:.2}", metrics.avg_score));
        }
        
        // Very few candidates
        if metrics.candidates_considered < metrics.recommendations_returned * 2 {
            issues.push("Too few candidates for quality filtering".to_string());
        }
        
        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_diversity_score() {
        // Perfect diversity: all unique creators
        assert!(QualityAnalyzer::diversity_score(10, 30, 10) > 0.9);
        
        // Low diversity: few unique creators
        assert!(QualityAnalyzer::diversity_score(2, 30, 10) < 0.3);
        
        // Medium diversity
        let score = QualityAnalyzer::diversity_score(5, 20, 10);
        assert!(score > 0.4 && score < 0.7);
    }
    
    #[test]
    fn test_personalization_score() {
        // High personalization: most recs match preferences
        assert!(QualityAnalyzer::personalization_score(5, 3, 2, 10) >= 0.9);
        
        // Low personalization: few matches
        assert!(QualityAnalyzer::personalization_score(1, 0, 1, 10) < 0.3);
    }
    
    #[test]
    fn test_detect_issues() {
        let mut metrics = RecommendationMetrics::default();
        metrics.recommendations_returned = 10;
        metrics.unique_creators = 2;
        metrics.unique_tags = 5;
        metrics.total_duration_ms = 250;
        metrics.discovery_count = 8;
        metrics.avg_score = 0.2;
        
        let issues = QualityAnalyzer::detect_issues(&metrics);
        assert!(!issues.is_empty());
        assert!(issues.iter().any(|i| i.contains("Low diversity")));
        assert!(issues.iter().any(|i| i.contains("Slow response")));
        assert!(issues.iter().any(|i| i.contains("High discovery")));
        assert!(issues.iter().any(|i| i.contains("Low avg score")));
    }
}
