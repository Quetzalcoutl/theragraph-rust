-- User Preferences: Track what content types/tags each user prefers
-- Updated based on their likes, purchases, and view time
CREATE TABLE IF NOT EXISTS user_preferences (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_address VARCHAR(42) NOT NULL,
    
    -- Content type preferences (0.0 to 1.0 weight)
    snap_affinity REAL NOT NULL DEFAULT 0.5,
    art_affinity REAL NOT NULL DEFAULT 0.5,
    music_affinity REAL NOT NULL DEFAULT 0.5,
    flix_affinity REAL NOT NULL DEFAULT 0.5,
    
    -- Tag preferences stored as JSONB for flexibility
    -- Format: {"abstract": 0.8, "nature": 0.7, "rock": 0.6, ...}
    tag_preferences JSONB NOT NULL DEFAULT '{}',
    
    -- Creator preferences (addresses of creators they engage with)
    -- Format: {"0x123...": 0.9, "0x456...": 0.7, ...}
    creator_preferences JSONB NOT NULL DEFAULT '{}',
    
    -- Behavioral signals
    total_likes INTEGER NOT NULL DEFAULT 0,
    total_purchases INTEGER NOT NULL DEFAULT 0,
    total_views INTEGER NOT NULL DEFAULT 0,
    avg_view_duration_ms BIGINT NOT NULL DEFAULT 0,
    
    -- Time-based decay factor (recent activity weighted more)
    last_activity_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    
    inserted_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    
    UNIQUE(user_address)
);

-- User Interactions: Log all user interactions for preference learning
CREATE TABLE IF NOT EXISTS user_interactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_address VARCHAR(42) NOT NULL,
    nft_id UUID NOT NULL,
    
    -- Interaction type: 'view', 'like', 'unlike', 'purchase', 'share', 'save'
    interaction_type VARCHAR(20) NOT NULL,
    
    -- Additional context
    view_duration_ms BIGINT,  -- How long they viewed (for 'view' type)
    source VARCHAR(50),       -- 'feed', 'enhanced_feed', 'profile', 'search'
    
    -- NFT snapshot at interaction time (for learning)
    nft_contract_type VARCHAR(20),
    nft_creator_address VARCHAR(42),
    nft_tags TEXT[],          -- Tags extracted from metadata
    
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- NFT Features: Extracted features from NFT metadata for matching
CREATE TABLE IF NOT EXISTS nft_features (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    nft_id UUID NOT NULL UNIQUE,
    contract_address VARCHAR(42) NOT NULL,
    token_id BIGINT NOT NULL,
    
    -- Extracted features
    tags TEXT[] NOT NULL DEFAULT '{}',
    primary_color VARCHAR(20),
    style VARCHAR(50),           -- 'abstract', 'realistic', 'cartoon', etc.
    mood VARCHAR(50),            -- 'happy', 'melancholic', 'energetic', etc.
    genre VARCHAR(50),           -- For music: 'rock', 'jazz', 'electronic', etc.
    
    -- Engagement metrics (updated periodically)
    engagement_score REAL NOT NULL DEFAULT 0.0,  -- Normalized 0-1
    trending_score REAL NOT NULL DEFAULT 0.0,    -- Time-decayed engagement
    quality_score REAL NOT NULL DEFAULT 0.5,     -- Based on creator reputation
    
    -- Content analysis (can be populated by ML later)
    embedding_vector REAL[],     -- For similarity matching
    
    inserted_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Recommendation Cache: Pre-computed recommendations for fast serving
CREATE TABLE IF NOT EXISTS recommendation_cache (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_address VARCHAR(42) NOT NULL,
    feed_type VARCHAR(20) NOT NULL,  -- 'following', 'enhanced', 'trending'
    
    -- Cached recommendations (ordered list of NFT IDs with scores)
    -- Format: [{"nft_id": "uuid", "score": 0.95, "reason": "similar_to_liked"}, ...]
    recommendations JSONB NOT NULL DEFAULT '[]',
    
    -- Cache metadata
    computed_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMP NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    
    UNIQUE(user_address, feed_type)
);

-- Trending snapshots: Track trending content over time windows
CREATE TABLE IF NOT EXISTS trending_snapshots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    time_window VARCHAR(20) NOT NULL,  -- 'hour', 'day', 'week'
    contract_type VARCHAR(20),          -- NULL for all types
    
    -- Top NFTs for this window
    -- Format: [{"nft_id": "uuid", "score": 100, "likes": 50, "buys": 10}, ...]
    top_nfts JSONB NOT NULL DEFAULT '[]',
    
    computed_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Indexes for fast queries
CREATE INDEX IF NOT EXISTS idx_user_preferences_address ON user_preferences(user_address);
CREATE INDEX IF NOT EXISTS idx_user_interactions_user ON user_interactions(user_address);
CREATE INDEX IF NOT EXISTS idx_user_interactions_nft ON user_interactions(nft_id);
CREATE INDEX IF NOT EXISTS idx_user_interactions_created ON user_interactions(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_nft_features_nft ON nft_features(nft_id);
CREATE INDEX IF NOT EXISTS idx_nft_features_tags ON nft_features USING GIN(tags);
DO $$
BEGIN
  IF EXISTS (
    SELECT 1 FROM information_schema.columns
    WHERE table_name = 'recommendation_cache' AND column_name = 'feed_type'
  ) THEN
    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_recommendation_cache_user ON recommendation_cache(user_address, feed_type)';
  ELSE
    EXECUTE 'CREATE INDEX IF NOT EXISTS idx_recommendation_cache_user ON recommendation_cache(user_address)';
  END IF;
END
$$;
CREATE INDEX IF NOT EXISTS idx_recommendation_cache_expires ON recommendation_cache(expires_at);
CREATE INDEX IF NOT EXISTS idx_trending_snapshots_window ON trending_snapshots(time_window, contract_type);
