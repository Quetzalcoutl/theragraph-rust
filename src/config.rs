#![allow(dead_code)]
//! Configuration management for TheraGraph Engine
//!
//! Provides strongly-typed configuration with validation, environment variable parsing,
//! and sensible defaults. Supports both development and production environments.
//!
//! # Example
//! ```no_run
//! use theragraph::Config;
//! let config = Config::from_env().expect("failed to load config");
//! println!("RPC URL: {}", config.blockchain.rpc_url);
//! ```

use crate::error::{Error, Result};
use std::time::Duration;
use tracing::info;

/// Main application configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Blockchain/RPC configuration
    pub blockchain: BlockchainConfig,
    /// Kafka configuration
    pub kafka: KafkaConfig,
    /// Database configuration
    pub database: DatabaseConfig,
    /// Elixir database configuration (for NFT count updates)
    pub elixir_database: DatabaseConfig,
    /// API server configuration
    pub api: ApiConfig,
    /// Contract addresses
    pub contracts: ContractAddresses,
    /// Recommendation engine configuration
    pub recommendation: RecommendationConfig,
}

/// Blockchain RPC configuration
#[derive(Debug, Clone)]
pub struct BlockchainConfig {
    /// RPC endpoint URL
    pub rpc_url: String,
    /// Chain ID (e.g., 100 for Gnosis)
    pub chain_id: u64,
    /// Block to start indexing from
    pub start_block: u64,
    /// Polling interval for new blocks
    pub poll_interval: Duration,
    /// Maximum blocks to process per batch
    pub batch_size: u64,
    /// Maximum retries for RPC calls
    pub max_retries: u32,
    /// Base delay for exponential backoff
    pub retry_delay: Duration,
}

/// Kafka configuration
#[derive(Debug, Clone)]
pub struct KafkaConfig {
    /// Kafka broker addresses (comma-separated)
    pub brokers: String,
    /// Consumer group ID
    pub group_id: String,
    /// Topics to produce to
    pub topics: KafkaTopics,
    /// Producer configuration
    pub producer: KafkaProducerConfig,
    /// Whether Kafka is enabled
    pub enabled: bool,
}

/// Kafka topic names
#[derive(Debug, Clone)]
pub struct KafkaTopics {
    pub blockchain_events: String,
    pub user_actions: String,
    pub recommendations: String,
}

/// Kafka producer configuration
#[derive(Debug, Clone)]
pub struct KafkaProducerConfig {
    /// Message timeout
    pub message_timeout: Duration,
    /// Delivery timeout (how long a single produce call waits for delivery)
    pub delivery_timeout: Duration,
    /// Maximum message size in bytes
    pub max_message_bytes: usize,
    /// Batch size for producer
    pub batch_size: usize,
    /// Linger time before sending batch
    pub linger: Duration,
    /// Compression type (none, gzip, snappy, lz4, zstd)
    pub compression: String,
    /// Acknowledgment level (0, 1, all)
    pub acks: String,
    /// Enable idempotent producer
    pub idempotent: bool,
    /// Reconnect backoff in ms
    pub reconnect_backoff_ms: u64,
    /// Reconnect backoff max in ms
    pub reconnect_backoff_max_ms: u64,
    /// Retries (if not using infinite retries)
    pub retries: u32,
    /// Optional librdkafka debug categories
    pub rdkafka_debug: Option<String>,
    /// Number of local send attempts before giving up
    pub send_max_attempts: u32,
    /// Base backoff in ms for send retries (exponential)
    pub send_backoff_base_ms: u64,
}

/// Database configuration
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL
    pub url: String,
    /// Maximum connections in pool
    pub max_connections: u32,
    /// Minimum connections to keep open
    pub min_connections: u32,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Idle timeout for connections
    pub idle_timeout: Duration,
    /// Maximum lifetime for connections
    pub max_lifetime: Duration,
    /// Enable statement caching
    pub statement_cache_size: usize,
}

/// API server configuration
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Port to listen on
    pub port: u16,
    /// Host to bind to
    pub host: String,
    /// Request timeout
    pub request_timeout: Duration,
    /// Maximum request body size
    pub max_body_size: usize,
    /// Enable CORS
    pub cors_enabled: bool,
    /// Allowed origins for CORS
    pub cors_origins: Vec<String>,
}

/// Contract addresses
#[derive(Debug, Clone)]
pub struct ContractAddresses {
    pub thera_friends: String,
    pub thera_social: String,
}

// ============================================================================
// TheraFriends Contract Constants
// ============================================================================

/// TheraFriends contract address on Sepolia testnet
pub const THERA_FRIENDS: &str = "0x280b971f9405aD604a4EaE50F3AD65Aa092F9f35";

/// ContentLiked event signature for TheraFriends contract
pub const FRIENDS_CONTENT_LIKED_SIG: &str =
    "0x8417b49947e6fe4baaaf043fd8bc39e9a14bdfcac1627dc1c35f75a8e844321b";

/// ContentUnliked event signature for TheraFriends contract
pub const FRIENDS_CONTENT_UNLIKED_SIG: &str =
    "0x54a63e587e58f95e1fb1b3a87102a23fac1fa5dd3d99442cc97043cf031b8ac1";

/// ContentCommented event signature for TheraFriends contract
pub const FRIENDS_CONTENT_COMMENTED_SIG: &str =
    "0x505d1203546d4a3699987fc90279e0a1dfe65117be15cac29d00ca3ed7a673b6";

/// ContentBlocked event signature for TheraFriends contract
pub const FRIENDS_CONTENT_BLOCKED_SIG: &str =
    "0x62d3506db24551831d906a4161625343e801105b08beef50f2616a51fd17a7b8";

/// ContentBookmarked event signature for TheraFriends contract
pub const FRIENDS_CONTENT_BOOKMARKED_SIG: &str =
    "0x4bbdc3b759094c64d5ae0d8d46654078d43716a6188ae8eb6bc36de1d06994c1";

/// UserFollowed event signature for TheraFriends contract
pub const FRIENDS_USER_FOLLOWED_SIG: &str =
    "0x53e62c84b456cda6228f6c0acd671088271c8bb9627a72d3f8c3d631c8473724";

/// UserUnfollowed event signature for TheraFriends contract
pub const FRIENDS_USER_UNFOLLOWED_SIG: &str =
    "0x594a48474c36e0d85b16b86393fc3d3a2ed770e7b4f0915b2972d5fbdaa99329";

/// UsernameRegistered event signature for TheraFriends contract
pub const FRIENDS_USERNAME_REGISTERED_SIG: &str =
    "0x0a09fa67e91ea818e53d712f63caf32f685bed0c54acdb1cebf8f63a36b454aa";

/// ProfileUpdated event signature for TheraFriends contract
pub const FRIENDS_PROFILE_UPDATED_SIG: &str =
    "0xdcb94c0b2c025b0736b4b62b1c595f2ca7ad4c711eada6026d477e87de9cca08";

/// UserVerified event signature for TheraFriends contract
pub const FRIENDS_USER_VERIFIED_SIG: &str =
    "0x22b3126528cda4618d13b6945f5e96fe53a5125f386aa591ee89134e2681c621";

/// UserBlocked event signature for TheraFriends contract
pub const FRIENDS_USER_BLOCKED_SIG: &str =
    "0x4906653113399be7fcd9c1ea679e52a58c1efeb96169aaa8b1fd94339ce12b57";

/// RoyaltyDistributed event signature for TheraFriends contract
pub const FRIENDS_ROYALTY_DISTRIBUTED_SIG: &str =
    "0xe3698e4763ee4becca0f71e44047f2c0018e133a8c70ab056c2ad3641fefd54a";

/// EarningsWithdrawn event signature for TheraFriends contract
pub const FRIENDS_EARNINGS_WITHDRAWN_SIG: &str =
    "0x90dac969af4a4897610ef8f0cd934c54409861eb7bd2205e552f8f2296ee5d3e";

/// ContentRequirementsUpdated event signature for TheraFriends contract
pub const FRIENDS_CONTENT_REQUIREMENTS_UPDATED_SIG: &str =
    "0xff02d2c736810756fea3a252038a4e88a63bf500d03dc6e5aeccf306963f9757";

/// ContentBurned event signature for TheraFriends contract
pub const FRIENDS_CONTENT_BURNED_SIG: &str =
    "0x528a31b859c72723f16bde373bc45e6e13a4d24d709e07200855baccec618cff";

/// BurnedContentRevenue event signature for TheraFriends contract
pub const FRIENDS_BURNED_CONTENT_REVENUE_SIG: &str =
    "0xc83ca0840994260dfd9b90ce0f552ac8a0424cae524b6dee6b476a78f6fbdc30";

/// TreasuryUpdated event signature for TheraFriends contract
pub const FRIENDS_TREASURY_UPDATED_SIG: &str =
    "0x08031759b0a2a99f63000784e546d7320d30692b97de1ea89a1645380cfb16f8";

/// DailyLimitsUpdated event signature for TheraFriends contract
pub const FRIENDS_DAILY_LIMITS_UPDATED_SIG: &str =
    "0x8c2ba571b537bdaa6702790f86f4a470d37ecd91a6d1e57acc410a039d4f6593";

/// PricesUpdated event signature for TheraFriends contract
pub const FRIENDS_PRICES_UPDATED_SIG: &str =
    "0xef8551c2f2bda52791575f92d96d767b28f44788358576de70e6c88d0c155ee9";

/// TokensRecovered event signature for TheraFriends contract
pub const FRIENDS_TOKENS_RECOVERED_SIG: &str =
    "0x382768820017a6e69506da8e35e39b17315306885e94830a6b4d97aa3e3587ff";

/// ContentMinted event signature for TheraFriends contract
pub const FRIENDS_CONTENT_MINTED_SIG: &str =
    "0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31";

/// ContentCopyMinted event signature for TheraFriends contract
pub const FRIENDS_CONTENT_COPY_MINTED_SIG: &str =
    "0x80c2e061ec45ed7331a60555bbadc701bd26c6335bcd10063bc2fe287d040f2f";

/// Starting block for TheraFriends contract indexing
pub const START_BLOCK: u64 = 9903816;

/// Recommendation engine configuration
#[derive(Debug, Clone)]
pub struct RecommendationConfig {
    /// Cache TTL for recommendations
    pub cache_ttl: Duration,
    /// Maximum candidates to consider
    pub max_candidates: usize,
    /// Minimum score threshold
    pub min_score: f32,
    /// Diversity factor (0.0-1.0)
    pub diversity_factor: f32,
    /// How often to update trending scores
    pub trending_update_interval: Duration,
    /// How often to update engagement scores
    pub engagement_update_interval: Duration,
    /// Preference decay rate
    pub preference_decay_rate: f32,
}

impl Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self> {
        // Prefer loading env from a directory of files (FFOLDER) for platforms that mount secrets as files.
        // Each file name is the env var name and its contents is the value.
        if let Ok(folder) = std::env::var("FFOLDER") {
            let p = std::path::Path::new(&folder);
            if p.is_dir() {
                match std::fs::read_dir(p) {
                    Ok(entries) => {
                        for entry in entries {
                            if let Ok(e) = entry {
                                if let Ok(fname) = e.file_name().into_string() {
                                    let fpath = e.path();
                                    if fpath.is_file() {
                                        if let Ok(mut contents) = std::fs::read_to_string(&fpath) {
                                            // Trim trailing newlines/spaces
                                            contents = contents.trim().to_string();
                                            // Only set env var if not already set in the environment
                                            if std::env::var(&fname).is_err() {
                                                std::env::set_var(&fname, contents);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => {
                        log::warn!("Failed to read FFOLDER {}: {}", folder, err);
                    }
                }
                log::info!("Loaded configuration from FFOLDER={}", folder);
            }
        } else {
            // Try to load .env file (ignore if not found)
            dotenvy::dotenv().ok();
        }

        let config = Self {
            blockchain: BlockchainConfig::from_env()?,
            kafka: KafkaConfig::from_env()?,
            database: DatabaseConfig::from_env()?,
            elixir_database: DatabaseConfig::from_env_elixir()?,
            api: ApiConfig::from_env()?,
            contracts: ContractAddresses::from_env()?,
            recommendation: RecommendationConfig::from_env()?,
        };

        config.validate()?;
        config.log_summary();

        Ok(config)
    }

    /// Validate configuration
    fn validate(&self) -> Result<()> {
        // Validate blockchain config
        if self.blockchain.rpc_url.is_empty() {
            return Err(Error::InvalidConfig {
                key: "RPC_URL",
                message: "RPC URL cannot be empty".into(),
            });
        }

        // Validate only the active contract addresses (social + friends)
        for (name, addr) in [
            ("THERA_FRIEND_ADDRESS", &self.contracts.thera_friends),
            ("THERA_SOCIAL_ADDRESS", &self.contracts.thera_social),
        ] {
            if !addr.starts_with("0x") || addr.len() != 42 {
                return Err(Error::InvalidConfig {
                    key: name,
                    message: format!("Invalid Ethereum address: {}", addr).into(),
                });
            }
        }

        // Validate pool size
        if self.database.max_connections < self.database.min_connections {
            return Err(Error::InvalidConfig {
                key: "DB_MAX_CONNECTIONS",
                message: "max_connections must be >= min_connections".into(),
            });
        }

        Ok(())
    }

    /// Log configuration summary (without sensitive data)
    fn log_summary(&self) {
        info!("Configuration loaded:");
        info!("  Blockchain:");
        info!("    RPC URL: {}", mask_url(&self.blockchain.rpc_url));
        info!("    Chain ID: {}", self.blockchain.chain_id);
        info!("    Start Block: {}", self.blockchain.start_block);
        info!("    Poll Interval: {:?}", self.blockchain.poll_interval);
        info!("  Database:");
        info!(
            "    Pool Size: {}-{}",
            self.database.min_connections, self.database.max_connections
        );
        info!("  API:");
        info!("    Listening on: {}:{}", self.api.host, self.api.port);
        info!("  Kafka:");
        info!("    Enabled: {}", self.kafka.enabled);
        if self.kafka.enabled {
            info!("    Brokers: {}", self.kafka.brokers);
        }
    }
}

impl BlockchainConfig {
    fn from_env() -> Result<Self> {
        Ok(Self {
            rpc_url: get_env("RPC_URL")?,
            chain_id: get_env_parsed("CHAIN_ID")?,
            start_block: get_env_or("START_BLOCK", &START_BLOCK.to_string())
                .parse()
                .unwrap_or(START_BLOCK),
            poll_interval: Duration::from_millis(
                get_env_or("POLL_INTERVAL_MS", "2000")
                    .parse()
                    .unwrap_or(2000),
            ),
            batch_size: get_env_or("BLOCK_BATCH_SIZE", "1000")
                .parse()
                .unwrap_or(1000),
            max_retries: get_env_or("RPC_MAX_RETRIES", "3").parse().unwrap_or(3),
            retry_delay: Duration::from_millis(
                get_env_or("RPC_RETRY_DELAY_MS", "1000")
                    .parse()
                    .unwrap_or(1000),
            ),
        })
    }
}

impl KafkaConfig {
    fn from_env() -> Result<Self> {
        let enabled = get_env_or("KAFKA_ENABLED", "true").parse().unwrap_or(true);

        Ok(Self {
            brokers: get_env_or("KAFKA_BROKERS", "kafka:29092"),
            group_id: get_env_or("KAFKA_GROUP_ID", "theragraph-engine"),
            enabled,
            topics: KafkaTopics {
                blockchain_events: get_env_or("KAFKA_TOPIC_BLOCKCHAIN", "blockchain.events"),
                user_actions: get_env_or("KAFKA_TOPIC_USER_ACTIONS", "user.actions"),
                recommendations: get_env_or("KAFKA_TOPIC_RECOMMENDATIONS", "recommendations"),
            },
            producer: KafkaProducerConfig {
                message_timeout: Duration::from_millis(
                    get_env_or("KAFKA_MESSAGE_TIMEOUT_MS", "5000")
                        .parse()
                        .unwrap_or(5000),
                ),
                delivery_timeout: Duration::from_millis(
                    get_env_or("KAFKA_DELIVERY_TIMEOUT_MS", "120000")
                        .parse()
                        .unwrap_or(120000),
                ),
                max_message_bytes: get_env_or("KAFKA_MAX_MESSAGE_BYTES", "20971520")
                    .parse()
                    .unwrap_or(20 * 1024 * 1024),
                batch_size: get_env_or("KAFKA_BATCH_SIZE", "16384")
                    .parse()
                    .unwrap_or(16384),
                linger: Duration::from_millis(
                    get_env_or("KAFKA_LINGER_MS", "5").parse().unwrap_or(5),
                ),
                compression: get_env_or("KAFKA_COMPRESSION", "lz4"),
                acks: get_env_or("KAFKA_ACKS", "all"),
                idempotent: get_env_or("KAFKA_IDEMPOTENT", "true")
                    .parse()
                    .unwrap_or(true),
                reconnect_backoff_ms: get_env_or("KAFKA_RECONNECT_BACKOFF_MS", "1000")
                    .parse()
                    .unwrap_or(1000),
                reconnect_backoff_max_ms: get_env_or("KAFKA_RECONNECT_BACKOFF_MAX_MS", "10000")
                    .parse()
                    .unwrap_or(10000),
                retries: get_env_or("KAFKA_CLIENT_RETRIES", "2147483647")
                    .parse()
                    .unwrap_or(2147483647u32),
                rdkafka_debug: {
                    let s = get_env_or("KAFKA_RDKAFKA_DEBUG", "");
                    if s.is_empty() { None } else { Some(s) }
                },
                send_max_attempts: get_env_or("KAFKA_SEND_MAX_ATTEMPTS", "5").parse().unwrap_or(5),
                send_backoff_base_ms: get_env_or("KAFKA_SEND_BACKOFF_BASE_MS", "200").parse().unwrap_or(200),
            },
        })
    }
}

impl DatabaseConfig {
    fn from_env() -> Result<Self> {
        let url = get_env("DATABASE_URL").unwrap_or_else(|_| {
            let user = std::env::var("USER").unwrap_or_else(|_| "postgres".to_string());
            format!("postgres://{}@localhost/theragraph_dev", user)
        });

        Ok(Self {
            url,
            max_connections: get_env_or("DB_MAX_CONNECTIONS", "20").parse().unwrap_or(20),
            min_connections: get_env_or("DB_MIN_CONNECTIONS", "5").parse().unwrap_or(5),
            connect_timeout: Duration::from_secs(
                get_env_or("DB_CONNECT_TIMEOUT_SECS", "30")
                    .parse()
                    .unwrap_or(30),
            ),
            idle_timeout: Duration::from_secs(
                get_env_or("DB_IDLE_TIMEOUT_SECS", "600")
                    .parse()
                    .unwrap_or(600),
            ),
            max_lifetime: Duration::from_secs(
                get_env_or("DB_MAX_LIFETIME_SECS", "3600")
                    .parse()
                    .unwrap_or(3600),
            ),
            statement_cache_size: get_env_or("DB_STATEMENT_CACHE_SIZE", "100")
                .parse()
                .unwrap_or(100),
        })
    }

    fn from_env_elixir() -> Result<Self> {
        let url = get_env("ELIXIR_DATABASE_URL").unwrap_or_else(|_| {
            let user = std::env::var("USER").unwrap_or_else(|_| "postgres".to_string());
            format!("postgres://{}@localhost/therafoundationapp_dev", user)
        });

        Ok(Self {
            url,
            max_connections: get_env_or("ELIXIR_DB_MAX_CONNECTIONS", "10")
                .parse()
                .unwrap_or(10),
            min_connections: get_env_or("ELIXIR_DB_MIN_CONNECTIONS", "2")
                .parse()
                .unwrap_or(2),
            connect_timeout: Duration::from_secs(
                get_env_or("ELIXIR_DB_CONNECT_TIMEOUT_SECS", "30")
                    .parse()
                    .unwrap_or(30),
            ),
            idle_timeout: Duration::from_secs(
                get_env_or("ELIXIR_DB_IDLE_TIMEOUT_SECS", "600")
                    .parse()
                    .unwrap_or(600),
            ),
            max_lifetime: Duration::from_secs(
                get_env_or("ELIXIR_DB_MAX_LIFETIME_SECS", "3600")
                    .parse()
                    .unwrap_or(3600),
            ),
            statement_cache_size: get_env_or("ELIXIR_DB_STATEMENT_CACHE_SIZE", "50")
                .parse()
                .unwrap_or(50),
        })
    }
}

impl ApiConfig {
    fn from_env() -> Result<Self> {
        Ok(Self {
            port: get_env_or("API_PORT", "8080").parse().unwrap_or(8080),
            host: get_env_or("API_HOST", "0.0.0.0"),
            request_timeout: Duration::from_secs(
                get_env_or("API_REQUEST_TIMEOUT_SECS", "30")
                    .parse()
                    .unwrap_or(30),
            ),
            max_body_size: get_env_or("API_MAX_BODY_SIZE", "10485760")
                .parse()
                .unwrap_or(10 * 1024 * 1024),
            cors_enabled: get_env_or("API_CORS_ENABLED", "true")
                .parse()
                .unwrap_or(true),
            cors_origins: get_env_or("API_CORS_ORIGINS", "*")
                .split(',')
                .map(|s| s.trim().to_string())
                .collect(),
        })
    }
}

impl ContractAddresses {
    fn from_env() -> Result<Self> {
        // Accept multiple environment variable names for compatibility
        let friends_addr = std::env::var("THERA_FRIEND_ADDRESS")
            .or_else(|_| std::env::var("THERA_FRIENDS_ADDRESS"))
            .or_else(|_| std::env::var("FRIENDS_ADDRESS"))
            .unwrap_or_else(|_| THERA_FRIENDS.to_string());

        // THERA_SOCIAL_ADDRESS is deprecated; fall back to friends address if unset
        let social_addr =
            std::env::var("THERA_SOCIAL_ADDRESS").unwrap_or_else(|_| friends_addr.clone());

        Ok(Self {
            thera_friends: friends_addr,
            thera_social: social_addr,
        })
    }
}

impl RecommendationConfig {
    fn from_env() -> Result<Self> {
        Ok(Self {
            cache_ttl: Duration::from_secs(
                get_env_or("REC_CACHE_TTL_SECS", "300")
                    .parse()
                    .unwrap_or(300),
            ),
            max_candidates: get_env_or("REC_MAX_CANDIDATES", "1000")
                .parse()
                .unwrap_or(1000),
            min_score: get_env_or("REC_MIN_SCORE", "0.1").parse().unwrap_or(0.1),
            diversity_factor: get_env_or("REC_DIVERSITY_FACTOR", "0.2")
                .parse()
                .unwrap_or(0.2),
            trending_update_interval: Duration::from_secs(
                get_env_or("REC_TRENDING_UPDATE_SECS", "3600")
                    .parse()
                    .unwrap_or(3600),
            ),
            engagement_update_interval: Duration::from_secs(
                get_env_or("REC_ENGAGEMENT_UPDATE_SECS", "3600")
                    .parse()
                    .unwrap_or(3600),
            ),
            preference_decay_rate: get_env_or("REC_PREFERENCE_DECAY", "0.95")
                .parse()
                .unwrap_or(0.95),
        })
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Get required environment variable
fn get_env(key: &'static str) -> Result<String> {
    std::env::var(key).map_err(|_| Error::MissingEnvVar { var: key })
}

/// Get environment variable with default
fn get_env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Get and parse environment variable
fn get_env_parsed<T: std::str::FromStr>(key: &'static str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    let value = get_env(key)?;
    value.parse().map_err(|e: T::Err| Error::InvalidConfig {
        key,
        message: format!("Invalid value '{}': {}", value, e).into(),
    })
}

/// Mask sensitive parts of URL
fn mask_url(url: &str) -> String {
    // Mask password if present
    if let Some(at_pos) = url.find('@') {
        if let Some(colon_pos) = url[..at_pos].rfind(':') {
            let (before, after) = url.split_at(colon_pos + 1);
            let (_, rest) = after.split_at(at_pos - colon_pos - 1);
            return format!("{}****{}", before, rest);
        }
    }
    url.to_string()
}

// ============================================================================
// Legacy compatibility
// ============================================================================

// Keep backward compatibility with old Config structure
impl Config {
    pub fn rpc_url(&self) -> &str {
        &self.blockchain.rpc_url
    }

    pub fn kafka_brokers(&self) -> &str {
        &self.kafka.brokers
    }

    pub fn database_url(&self) -> &str {
        &self.database.url
    }

    // Legacy per-contract address getters removed; use `thera_social_address` instead

    pub fn thera_friend_address(&self) -> &str {
        &self.contracts.thera_friends
    }

    pub fn thera_social_address(&self) -> &str {
        &self.contracts.thera_social
    }

    pub fn start_block(&self) -> u64 {
        self.blockchain.start_block
    }

    pub fn poll_interval_ms(&self) -> u64 {
        self.blockchain.poll_interval.as_millis() as u64
    }
}
