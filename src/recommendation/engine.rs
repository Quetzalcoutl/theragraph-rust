//! Recommendation Engine
//!
//! Core algorithm for generating personalized NFT recommendations.
//! Combines user preferences, content features, social signals, and trending data.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::debug;

use super::features::NftFeatures;
use super::preferences::UserPreferences;

/// A scored recommendation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredNft {
    pub nft_id: String,
    pub score: f32,
    pub reason: RecommendationReason,
    pub contract_type: String,
    pub creator_address: String,
    pub tags: Vec<String>,
}

/// Why this NFT was recommended
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationReason {
    /// Matches user's tag preferences
    TagMatch { matching_tags: Vec<String> },
    /// From a creator user has engaged with
    CreatorAffinity { creator: String },
    /// Similar content type preference
    ContentTypeMatch { content_type: String },
    /// Currently trending
    Trending { trending_score: f32 },
    /// From someone user follows
    Following { followee: String },
    /// High quality/engagement
    HighEngagement { engagement_score: f32 },
    /// Serendipity - introducing variety
    Discovery,
}

/// Recommendation weights (can be tuned)
#[derive(Debug, Clone)]
pub struct ScoringWeights {
    pub tag_match: f32,
    pub creator_affinity: f32,
    pub content_type: f32,
    pub trending: f32,
    pub engagement: f32,
    pub quality: f32,
    pub recency: f32,
    pub diversity_penalty: f32,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            tag_match: 0.30,         // 30% weight on tag matching
            creator_affinity: 0.15,  // 15% weight on creator preference
            content_type: 0.15,      // 15% weight on content type match
            trending: 0.10,          // 10% weight on trending score
            engagement: 0.10,        // 10% weight on overall engagement
            quality: 0.10,           // 10% weight on quality score
            recency: 0.05,           // 5% weight on how new the NFT is
            diversity_penalty: 0.05, // 5% penalty for too similar items
        }
    }
}

/// Context used for scoring a single NFT
    pub struct ScoringContext<'a> {
        pub prefs: &'a UserPreferences,
        pub contract_type: &'a str,
        pub creator_address: &'a str,
        pub created_at: &'a str,
        pub features: &'a Option<NftFeatures>,
        pub seen_creators: &'a HashMap<String, usize>,
        pub seen_tags: &'a HashMap<String, usize>,
    }

    /// Main recommendation engine
#[derive(Clone)]
pub struct RecommendationEngine {
    pool: PgPool,
    weights: ScoringWeights,
}

impl RecommendationEngine {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            weights: ScoringWeights::default(),
        }
    }

    #[allow(dead_code)]
    pub fn with_weights(pool: PgPool, weights: ScoringWeights) -> Self {
        Self { pool, weights }
    }

    /// Get personalized enhanced feed for a user
    /// This is the main recommendation endpoint
    pub async fn get_enhanced_feed(
        &self,
        user_address: &str,
        limit: usize,
        offset: usize,
        contract_type_filter: Option<&str>,
    ) -> Result<Vec<ScoredNft>> {
        let prefs = super::preferences::get_or_create_preferences(&self.pool, user_address).await?;

        // Get candidate NFTs
        let candidates = self
            .get_candidates(contract_type_filter, limit * 3, offset)
            .await?;

        // Score each candidate
        let mut scored: Vec<ScoredNft> = Vec::with_capacity(candidates.len());
        let mut seen_creators: HashMap<String, usize> = HashMap::new();
        let mut seen_tags: HashMap<String, usize> = HashMap::new();

        for (nft, features) in candidates {
            let nft_id = match &nft.id {
                Some(id) => id.clone(),
                None => continue, // Skip NFTs without ID
            };
            let contract_type = nft.contract_type.clone().unwrap_or_default();
            let created_at = nft.created_at.clone().unwrap_or_default();

            let ctx = ScoringContext {
                prefs: &prefs,
                contract_type: &contract_type,
                creator_address: &nft.creator_address,
                created_at: &created_at,
                features: &features,
                seen_creators: &seen_creators,
                seen_tags: &seen_tags,
            };

            let (score, reason) = self.calculate_score(&ctx);

            // Track diversity
            if let Some(creator) = features.as_ref().map(|f| &f.contract_address) {
                *seen_creators.entry(creator.clone()).or_insert(0) += 1;
            }
            if let Some(f) = &features {
                for tag in &f.tags {
                    *seen_tags.entry(tag.clone()).or_insert(0) += 1;
                }
            }

            scored.push(ScoredNft {
                nft_id,
                score,
                reason,
                contract_type,
                creator_address: nft.creator_address.clone(),
                tags: features.map(|f| f.tags).unwrap_or_default(),
            });
        }

        // Sort by score descending
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Take top N with some randomization for discovery
        let result = self.apply_diversity_shuffle(scored, limit);

        debug!(
            "Generated {} recommendations for user {}",
            result.len(),
            user_address
        );

        Ok(result)
    }

    /// Get personalized recommendations for a user
    /// This is the main method called by the Elixir GraphQL API
    pub async fn get_recommendations(
        &self,
        user_address: &str,
        limit: usize,
        contract_type_filter: Option<&str>,
        exclude_seen: bool,
    ) -> Result<Vec<ScoredNft>> {
        // Check cache first
        if let Some(cached) =
            get_cached_recommendations(&self.pool, user_address, "personalized").await?
        {
            let total = cached.len();
            if total >= limit {
                return Ok(cached.into_iter().take(limit).collect());
            }
        }

        let prefs = super::preferences::get_or_create_preferences(&self.pool, user_address).await?;

        // Get candidate NFTs (more than needed for diversity)
        let candidates = self
            .get_candidates(contract_type_filter, limit * 4, 0)
            .await?;

        // Score each candidate
        let mut scored: Vec<ScoredNft> = Vec::with_capacity(candidates.len());
        let mut seen_creators: HashMap<String, usize> = HashMap::new();
        let mut seen_tags: HashMap<String, usize> = HashMap::new();

        for (nft, features) in candidates {
            let nft_id = match &nft.id {
                Some(id) => id.clone(),
                None => continue,
            };

            // Skip if user has already seen this NFT and exclude_seen is true
            if exclude_seen && self.has_user_seen_nft(user_address, &nft_id).await? {
                continue;
            }

            let contract_type = nft.contract_type.clone().unwrap_or_default();
            let creator_address = nft.creator_address.clone();
            let created_at = nft.created_at.clone().unwrap_or_default();

            let ctx = ScoringContext {
                prefs: &prefs,
                contract_type: &contract_type,
                creator_address: &creator_address,
                created_at: &created_at,
                features: &features,
                seen_creators: &seen_creators,
                seen_tags: &seen_tags,
            };

            let (score, reason) = self.calculate_score(&ctx);

            // Track seen creators/tags for diversity
            *seen_creators.entry(creator_address.clone()).or_insert(0) += 1;
            if let Some(tags) = features.as_ref().map(|f| &f.tags) {
                for tag in tags {
                    *seen_tags.entry(tag.clone()).or_insert(0) += 1;
                }
            }

            scored.push(ScoredNft {
                nft_id,
                score,
                reason,
                contract_type,
                creator_address,
                tags: features.map(|f| f.tags).unwrap_or_default(),
            });
        }

        // Sort by score descending
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Apply diversity and discovery
        let result = self.apply_diversity_shuffle(scored, limit);

        // Cache for 10 minutes
        let _ = cache_recommendations(&self.pool, user_address, "personalized", &result, 10).await;

        debug!(
            "Generated {} personalized recommendations for user {}",
            result.len(),
            user_address
        );

        Ok(result)
    }

    /// Get feed from followed users only
    pub async fn get_following_feed(
        &self,
        user_address: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ScoredNft>> {
        // Get list of addresses this user follows
        let following = self.get_following_addresses(user_address).await?;

        if following.is_empty() {
            return Ok(Vec::new());
        }

        // Get NFTs from followed creators
        let nfts = self
            .get_nfts_from_creators(&following, limit, offset)
            .await?;

        // Score them (simpler scoring for following feed - mostly chronological)
        let mut scored: Vec<ScoredNft> = Vec::new();

        for nft in nfts {
            let nft_id = match &nft.id {
                Some(id) => id.clone(),
                None => continue,
            };
            let contract_type = nft.contract_type.clone().unwrap_or_default();
            let created_at = nft.created_at.clone().unwrap_or_default();

            let features = self.get_nft_features(&nft_id).await?;

            // For following feed, score is mainly recency + engagement
            let recency_score = Self::compute_recency_score(&created_at);
            let engagement_score = features.as_ref().map(|f| f.engagement_score).unwrap_or(0.0);

            let score = recency_score * 0.7 + engagement_score * 0.3;

            scored.push(ScoredNft {
                nft_id,
                score,
                reason: RecommendationReason::Following {
                    followee: nft.creator_address.clone(),
                },
                contract_type,
                creator_address: nft.creator_address.clone(),
                tags: features.map(|f| f.tags).unwrap_or_default(),
            });
        }

        // Sort by score
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(scored)
    }

    /// Context used for scoring a single NFT


    // ---- Scoring helpers (pure functions) ----
    fn compute_type_affinity_score(weights: &ScoringWeights, contract_type: &str, prefs: &UserPreferences) -> (f32, Option<RecommendationReason>) {
        let type_affinity = match contract_type {
            "snap" => prefs.snap_affinity,
            "art" => prefs.art_affinity,
            "music" => prefs.music_affinity,
            "flix" => prefs.flix_affinity,
            _ => 0.5,
        };
        let type_score = type_affinity * weights.content_type;
        let reason = if type_score > 0.0 && type_affinity > 0.6 {
            Some(RecommendationReason::ContentTypeMatch { content_type: contract_type.to_string() })
        } else {
            None
        };
        (type_score, reason)
    }

    fn compute_creator_affinity_score(weights: &ScoringWeights, creator: &str, prefs: &UserPreferences) -> (f32, Option<RecommendationReason>) {
        let creator_affinity = prefs.creator_preferences.get(creator).copied().unwrap_or(0.0);
        let creator_score = creator_affinity * weights.creator_affinity;
        let reason = if creator_score > 0.0 && creator_affinity > 0.5 {
            Some(RecommendationReason::CreatorAffinity { creator: creator.to_string() })
        } else {
            None
        };
        (creator_score, reason)
    }

    fn compute_feature_scores(
        weights: &ScoringWeights,
        f: &NftFeatures,
        prefs: &UserPreferences,
        seen_creators: &HashMap<String, usize>,
        seen_tags: &HashMap<String, usize>,
    ) -> (f32, Option<RecommendationReason>) {
        let mut total = 0.0f32;
        let mut primary: Option<RecommendationReason> = None;
        let mut max_score = 0.0f32;

        // Tag matching
        let mut matching_tags = Vec::new();
        let mut tag_score_sum = 0.0f32;
        for tag in &f.tags {
            if let Some(&pref) = prefs.tag_preferences.get(tag) {
                if pref > 0.5 {
                    matching_tags.push(tag.clone());
                    tag_score_sum += pref;
                }
            }
        }
        let tag_match_score = if !matching_tags.is_empty() {
            (tag_score_sum / matching_tags.len() as f32) * weights.tag_match
        } else {
            0.0
        };
        total += tag_match_score;
        if tag_match_score > max_score && !matching_tags.is_empty() {
            max_score = tag_match_score;
            primary = Some(RecommendationReason::TagMatch { matching_tags });
        }

        // Trending
        let trending_contrib = f.trending_score * weights.trending;
        total += trending_contrib;
        if trending_contrib > max_score && f.trending_score > 0.5 {
            max_score = trending_contrib;
            primary = Some(RecommendationReason::Trending { trending_score: f.trending_score });
        }

        // Engagement
        let engagement_contrib = f.engagement_score * weights.engagement;
        total += engagement_contrib;
        if engagement_contrib > max_score && f.engagement_score > 0.7 {
            max_score = engagement_contrib;
            primary = Some(RecommendationReason::HighEngagement { engagement_score: f.engagement_score });
        }

        // Quality
        total += f.quality_score * weights.quality;

        // Diversity penalties
        let creator_count = seen_creators.get(&f.contract_address).copied().unwrap_or(0);
        if creator_count > 2 {
            total -= weights.diversity_penalty * (creator_count as f32 - 2.0) * 0.1;
        }

        let tag_oversaturation: f32 = f.tags.iter().map(|t| seen_tags.get(t).copied().unwrap_or(0) as f32).sum::<f32>() / f.tags.len().max(1) as f32;
        if tag_oversaturation > 5.0 {
            total -= weights.diversity_penalty * (tag_oversaturation - 5.0) * 0.02;
        }

        (total, primary)
    }

    fn calculate_score(&self, ctx: &ScoringContext<'_>) -> (f32, RecommendationReason) {
        let mut score = 0.0;
        let mut primary_reason = RecommendationReason::Discovery;
        let mut max_reason_score = 0.0f32;

        // 1. Content type affinity
        let (type_score, type_reason) = Self::compute_type_affinity_score(&self.weights, ctx.contract_type, ctx.prefs);
        score += type_score;
        if let Some(r) = type_reason {
            if type_score > max_reason_score {
                max_reason_score = type_score;
                primary_reason = r;
            }
        }

        // 2. Creator affinity
        let (creator_score, creator_reason) = Self::compute_creator_affinity_score(&self.weights, ctx.creator_address, ctx.prefs);
        score += creator_score;
        if let Some(r) = creator_reason {
            if creator_score > max_reason_score {
                max_reason_score = creator_score;
                primary_reason = r;
            }
        }

        // Feature-based scores
        if let Some(ref f) = ctx.features {
            let (feature_score, feature_reason) = Self::compute_feature_scores(&self.weights, f, ctx.prefs, ctx.seen_creators, ctx.seen_tags);
            score += feature_score;
            if let Some(r) = feature_reason {
                if feature_score > max_reason_score {
                    max_reason_score = feature_score;
                    primary_reason = r;
                }
            }
        }

        // 8. Recency bonus
let recency = Self::compute_recency_score(ctx.created_at);
        score += recency * self.weights.recency;

        // Clamp score to 0-1
        score = score.clamp(0.0, 1.0);

        (score, primary_reason)
    }

    fn calculate_recency_score(&self, created_at: &str) -> f32 {
        Self::compute_recency_score(created_at)
    }

    fn compute_recency_score(created_at: &str) -> f32 {
        // Parse timestamp and calculate decay
        // Newer = higher score
        match chrono::DateTime::parse_from_rfc3339(created_at) {
            Ok(dt) => {
                let age_hours =
                    (chrono::Utc::now() - dt.with_timezone(&chrono::Utc)).num_hours() as f32;
                // Exponential decay: half-life of 24 hours
                (-age_hours / 24.0).exp()
            }
            Err(_) => 0.5, // Default if parse fails
        }
    }

    /// Apply slight randomization to top results for discovery
    fn apply_diversity_shuffle(&self, mut scored: Vec<ScoredNft>, limit: usize) -> Vec<ScoredNft> {
        use rand::seq::SliceRandom;

        if scored.len() <= limit {
            return scored;
        }

        // Take top 80% deterministically, shuffle remaining 20% slots
        let deterministic_count = (limit as f32 * 0.8) as usize;
        let shuffle_count = limit - deterministic_count;

        let mut result: Vec<ScoredNft> = scored.drain(..deterministic_count).collect();

        // From remaining, pick some randomly for discovery
        let mut rng = rand::thread_rng();
        let remaining: Vec<_> = scored.into_iter().take(shuffle_count * 3).collect();

        if !remaining.is_empty() {
            let chosen: Vec<_> = remaining
                .choose_multiple(&mut rng, shuffle_count.min(remaining.len()))
                .cloned()
                .collect();
            result.extend(chosen);
        }

        result
    }

    /// Check if user has already seen/interacted with an NFT
    async fn has_user_seen_nft(&self, user_address: &str, nft_id: &str) -> Result<bool> {
        let result: Option<bool> = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM user_interactions 
                WHERE user_address = $1 
                AND nft_id = $2 
                AND interaction_type IN ('view', 'like', 'purchase', 'save')
                AND created_at > NOW() - INTERVAL '30 days'
            )
            "#,
        )
        .bind(user_address.to_lowercase())
        .bind(nft_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(result.unwrap_or(false))
    }



    // Database query helpers


    async fn get_candidates(
        &self,
        contract_type_filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<(CandidateNft, Option<NftFeatures>)>> {
        let nfts: Vec<CandidateNft> = if let Some(ct) = contract_type_filter {
            sqlx::query_as::<_, CandidateNft>(
                r#"
                SELECT id::text, contract_type::text, creator_address, 
                       creation_time::text as created_at
                FROM nfts 
                WHERE is_deleted = false 
                AND is_original = true
                AND contract_type = $1
                ORDER BY creation_time DESC
                LIMIT $2 OFFSET $3
                "#,
            )
            .bind(ct)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, CandidateNft>(
                r#"
                SELECT id::text, contract_type::text, creator_address,
                       creation_time::text as created_at
                FROM nfts 
                WHERE is_deleted = false 
                AND is_original = true
                ORDER BY creation_time DESC
                LIMIT $1 OFFSET $2
                "#,
            )
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(&self.pool)
            .await?
        };

        let mut results = Vec::with_capacity(nfts.len());
        for nft in nfts {
            let nft_id = match &nft.id {
                Some(id) => id.clone(),
                None => continue, // Skip NFTs without ID
            };
            let features = self.get_nft_features(&nft_id).await?;
            results.push((nft, features));
        }

        Ok(results)
    }

    async fn get_nft_features(&self, nft_id: &str) -> Result<Option<NftFeatures>> {
        super::features::get_features(&self.pool, nft_id).await
    }

    async fn get_following_addresses(&self, user_address: &str) -> Result<Vec<String>> {
        let rows = sqlx::query_scalar::<_, String>(
            r#"
            SELECT u.address 
            FROM follows f
            JOIN social_users u ON u.id = f.followee_id
            JOIN social_users follower ON follower.id = f.follower_id
            WHERE follower.address = $1 AND f.is_active = true
            "#,
        )
        .bind(user_address.to_lowercase())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    async fn get_nfts_from_creators(
        &self,
        creators: &[String],
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CandidateNft>> {
        let nfts = sqlx::query_as::<_, CandidateNft>(
            r#"
            SELECT id::text, contract_type::text, creator_address,
                   creation_time::text as created_at
            FROM nfts 
            WHERE is_deleted = false 
            AND is_original = true
            AND creator_address = ANY($1)
            ORDER BY creation_time DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(creators)
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(nfts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_compute_type_affinity_score_high_pref() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.art_affinity = 0.8;

        let (score, reason) = RecommendationEngine::compute_type_affinity_score(&weights, "art", &prefs);
        assert!(score > 0.0);
        match reason {
            Some(RecommendationReason::ContentTypeMatch { content_type }) => {
                assert_eq!(content_type, "art")
            }
            _ => panic!("expected content type match reason"),
        }
    }

    #[test]
    fn test_compute_feature_scores_tag_match() {
        let weights = ScoringWeights::default();
        let mut prefs = UserPreferences::default();
        prefs.tag_preferences.insert("landscape".to_string(), 0.8);

        let f = NftFeatures {
            nft_id: "1".to_string(),
            contract_address: "0xabc".to_string(),
            token_id: 1,
            tags: vec!["landscape".to_string()],
            primary_color: None,
            style: None,
            mood: None,
            genre: None,
            engagement_score: 0.0,
            trending_score: 0.0,
            quality_score: 0.0,
        };

        let seen_creators: HashMap<String, usize> = HashMap::new();
        let seen_tags: HashMap<String, usize> = HashMap::new();

        let (score, reason) = RecommendationEngine::compute_feature_scores(&weights, &f, &prefs, &seen_creators, &seen_tags);
        assert!(score > 0.0);
        match reason {
            Some(RecommendationReason::TagMatch { matching_tags }) => {
                assert_eq!(matching_tags, vec!["landscape".to_string()]);
            }
            _ => panic!("expected TagMatch reason"),
        }
    }

    #[test]
    fn test_compute_recency_score_recent_vs_old() {
        let now = chrono::Utc::now();
        let recent = now.to_rfc3339();
        let old = (now - chrono::Duration::days(10)).to_rfc3339();

        let r1 = RecommendationEngine::compute_recency_score(&recent);
        let r2 = RecommendationEngine::compute_recency_score(&old);
        assert!(r1 > r2);
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct CandidateNft {
    id: Option<String>,
    contract_type: Option<String>,
    creator_address: String,
    created_at: Option<String>,
}

/// Cache recommendations for faster serving
pub async fn cache_recommendations(
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
    recommendations: &[ScoredNft],
    ttl_minutes: i64,
) -> Result<()> {
    let expires_at = chrono::Utc::now() + chrono::Duration::minutes(ttl_minutes);
    let recommendations_json = serde_json::to_value(recommendations)?;

    sqlx::query(
        r#"
        INSERT INTO recommendation_cache 
            (id, user_address, feed_type, recommendations, computed_at, expires_at, version)
        VALUES 
            (gen_random_uuid(), $1, $2, $3, NOW(), $4, 1)
        ON CONFLICT (user_address, feed_type) DO UPDATE SET
            recommendations = $3,
            computed_at = NOW(),
            expires_at = $4,
            version = recommendation_cache.version + 1
        "#,
    )
    .bind(user_address.to_lowercase())
    .bind(feed_type)
    .bind(&recommendations_json)
    .bind(expires_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get cached recommendations if valid
pub async fn get_cached_recommendations(
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
) -> Result<Option<Vec<ScoredNft>>> {
    let result = sqlx::query_scalar::<_, serde_json::Value>(
        r#"
        SELECT recommendations 
        FROM recommendation_cache
        WHERE user_address = $1 
        AND feed_type = $2
        AND expires_at > NOW()
        "#,
    )
    .bind(user_address.to_lowercase())
    .bind(feed_type)
    .fetch_optional(pool)
    .await?;

    match result {
        Some(value) => Ok(serde_json::from_value(value)?),
        None => Ok(None),
    }
}
