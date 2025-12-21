# TheraGraph Recommendation System

A personalized NFT recommendation engine that learns user preferences and delivers tailored content feeds.

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────────┐
│                        Frontend (Next.js)                        │
│              EnhancedFeedView / FollowingFeed                    │
└─────────────────────────────┬────────────────────────────────────┘
                              │ HTTP API
                              ▼
┌──────────────────────────────────────────────────────────────────┐
│                   Rust Recommendation Engine                      │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │ API Server  │  │  Engine     │  │    Preference Tracker   │  │
│  │  (Axum)     │◄─┤  (Scoring)  │◄─┤  (User Behavior)        │  │
│  └─────────────┘  └─────────────┘  └─────────────────────────┘  │
│         │               │                    │                   │
│         └───────────────┴────────────────────┘                   │
│                         │                                        │
└─────────────────────────┼────────────────────────────────────────┘
                          │
                          ▼
┌──────────────────────────────────────────────────────────────────┐
│                      PostgreSQL Database                          │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │ user_       │  │ nft_        │  │ user_interactions       │  │
│  │ preferences │  │ features    │  │ (like, view, purchase)  │  │
│  └─────────────┘  └─────────────┘  └─────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

## API Endpoints

### Enhanced Feed (Personalized)
```
GET /api/v1/feed/enhanced/:user_address?limit=20&offset=0&type=snap
```

Returns NFTs scored and ranked based on:
- User's content type preferences (snap, art, music, flix)
- Tag matching from past interactions
- Creator affinity from follows and engagements
- Trending and engagement scores
- Diversity balancing

### Following Feed
```
GET /api/v1/feed/following/:user_address?limit=20&offset=0
```

Returns NFTs from creators the user follows, sorted by recency with engagement boost.

### Record Interaction
```
POST /api/v1/interactions
{
  "user_address": "0x...",
  "nft_id": "uuid",
  "interaction_type": "like|view|purchase|share|save|unlike",
  "view_duration_ms": 5000,
  "source": "enhanced_feed"
}
```

Records user interactions to update preference profiles.

## Scoring Algorithm

The enhanced feed uses a multi-factor scoring system:

| Factor | Weight | Description |
|--------|--------|-------------|
| Tag Match | 30% | NFTs with tags user has engaged with |
| Content Type | 15% | User's preference for snap/art/music/flix |
| Creator Affinity | 15% | Creators user has engaged with before |
| Trending | 10% | Currently popular content |
| Engagement | 10% | Overall engagement level of NFT |
| Quality | 10% | Creator reputation score |
| Recency | 5% | Newer content bonus |
| Diversity | 5% | Penalty for too much from same creator/tags |

### Score Calculation

```rust
score = tag_match * 0.30 +
        content_type * 0.15 +
        creator_affinity * 0.15 +
        trending * 0.10 +
        engagement * 0.10 +
        quality * 0.10 +
        recency * 0.05 -
        diversity_penalty * 0.05
```

## User Preference Learning

### Interaction Weights
- **Purchase**: 3.0 (strongest signal)
- **Like**: 1.0
- **Save**: 0.7
- **Share**: 0.5
- **Long View (>5s)**: 0.3
- **View**: 0.1
- **Unlike**: -0.5 (negative signal)

### Preference Decay
Preferences decay daily (0.95 factor) towards neutral (0.5) to keep recommendations fresh and adapt to changing interests.

## Database Schema

### user_preferences
```sql
CREATE TABLE user_preferences (
    id UUID PRIMARY KEY,
    user_address TEXT UNIQUE NOT NULL,
    snap_affinity REAL DEFAULT 0.5,
    art_affinity REAL DEFAULT 0.5,
    music_affinity REAL DEFAULT 0.5,
    flix_affinity REAL DEFAULT 0.5,
    tag_preferences JSONB DEFAULT '{}',
    creator_preferences JSONB DEFAULT '{}',
    total_likes INTEGER DEFAULT 0,
    total_purchases INTEGER DEFAULT 0,
    total_views INTEGER DEFAULT 0,
    last_activity_at TIMESTAMP,
    inserted_at TIMESTAMP DEFAULT NOW(),
    updated_at TIMESTAMP DEFAULT NOW()
);
```

### nft_features
```sql
CREATE TABLE nft_features (
    id UUID PRIMARY KEY,
    nft_id UUID UNIQUE NOT NULL REFERENCES nfts(id),
    contract_address TEXT NOT NULL,
    token_id BIGINT NOT NULL,
    tags TEXT[] DEFAULT '{}',
    primary_color TEXT,
    style TEXT,
    mood TEXT,
    genre TEXT,
    engagement_score REAL DEFAULT 0,
    trending_score REAL DEFAULT 0,
    quality_score REAL DEFAULT 0,
    inserted_at TIMESTAMP DEFAULT NOW(),
    updated_at TIMESTAMP DEFAULT NOW()
);
```

### user_interactions
```sql
CREATE TABLE user_interactions (
    id UUID PRIMARY KEY,
    user_address TEXT NOT NULL,
    nft_id UUID NOT NULL REFERENCES nfts(id),
    interaction_type TEXT NOT NULL,
    view_duration_ms BIGINT,
    source TEXT,
    nft_contract_type TEXT,
    nft_creator_address TEXT,
    nft_tags TEXT[] DEFAULT '{}',
    created_at TIMESTAMP DEFAULT NOW()
);
```

## Running the Engine

### Development
```bash
cd theragraph-rust
cargo run
```

### Production
```bash
cargo build --release
./target/release/theragraph-engine
```

### Environment Variables
```env
DATABASE_URL=postgres://user:pass@host/db
KAFKA_BROKERS=localhost:9092
RPC_URL=https://polygon-mainnet.g.alchemy.com/v2/xxx
API_PORT=8080
```

## Integration with Frontend

Update the EnhancedFeedView component to fetch from the Rust API:

```typescript
const fetchEnhancedFeed = async (address: string, limit = 20, offset = 0) => {
  const response = await fetch(
    `http://localhost:8080/api/v1/feed/enhanced/${address}?limit=${limit}&offset=${offset}`
  );
  return response.json();
};
```

## Scheduled Jobs

The engine runs background tasks:

1. **Score Updates** (hourly)
   - Recalculate engagement scores from likes/buys
   - Update trending scores from recent activity
   - Apply preference decay for inactive users

2. **Feature Extraction** (on NFT creation)
   - Extract tags, style, mood, genre from metadata
   - Calculate initial quality score

## Monitoring

Key metrics to track:
- API response times (p50, p95, p99)
- Cache hit rates
- Score distribution (avoid bubble effects)
- Diversity metrics (creator/tag concentration)
- User engagement with recommendations

## Future Improvements

1. **Collaborative Filtering**: Find similar users and recommend what they like
2. **Content-Based Similarity**: Use embeddings for semantic matching
3. **A/B Testing**: Test different weight configurations
4. **Cold Start**: Better recommendations for new users
5. **Explainability**: Show users why something was recommended
