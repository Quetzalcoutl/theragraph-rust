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
