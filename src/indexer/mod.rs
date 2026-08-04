//! Blockchain Indexers
//!
//! Each indexer polls a specific smart contract for events and processes them.
//! Events are published to Kafka for downstream consumers.
//!
//! # Indexers
//!
//! - `thera_friendz` (a re-export of `thera_social`) - the sole indexer for the
//!   unified TheraFriendz contract (content + social events). A second,
//!   `friend` indexer used to also poll this exact same contract address with
//!   the identical event parser — differing only in its cursor name — so every
//!   event was processed twice from two independent block cursors. Removed;
//!   see the comment on `spawn_indexers` in main.rs for the full history.

pub mod thera_friendz;
pub mod thera_social;

use crate::error::{Error, Result};
use crate::kafka::KafkaProducer;
use ethers::prelude::*;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{error, info, instrument, warn};

// ── Generic blockchain indexer ────────────────────────────────────────────────

/// Contract-specific identity for the generic indexer.
///
/// Implement this for each contract to eliminate the run-loop boilerplate.
/// `FriendsContractType` and `TheraSocialContractType` are the two concrete
/// adapters — that makes this a real seam, not hypothetical.
pub trait ContractType: Send + Sync + 'static {
    /// Key used in `indexer_state` table (e.g. "friend", "friends").
    fn cursor_type_name(&self) -> &'static str;
    /// Contract type string passed to `crate::events::parse_log`.
    fn event_parse_type(&self) -> &'static str;
    /// Human-readable name for log messages.
    fn display_name(&self) -> &'static str;
}

/// One indexer instance for any `ContractType`.
///
/// Holds all shared state; run-loop, batch fetch, and log dispatch are
/// implemented once here. `run_with_state` wrappers in `friend.rs` /
/// `thera_social.rs` are the only per-contract code.
pub struct GenericIndexer<C: ContractType> {
    pub contract: C,
    pub provider: Arc<Provider<Http>>,
    pub contract_address: Address,
    pub kafka: KafkaProducer,
    pub pool: PgPool,
    pub poll_interval: Duration,
    pub batch_size: u64,
    pub current_block: u64,
    pub request_delay: Duration,
    pub max_retries: u32,
    pub retry_delay: Duration,
    /// Direct preference-signal handler used when Kafka is disabled.
    pub direct: Option<Arc<crate::event_processor::DirectHandlers>>,
}

impl<C: ContractType> GenericIndexer<C> {
    pub async fn run(&mut self, shutdown_rx: &mut broadcast::Receiver<()>) -> Result<()> {
        info!(
            "{} started for contract: {:?}",
            self.contract.display_name(),
            self.contract_address
        );
        info!("📍 Starting from block: {}", self.current_block);

        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => {
                    info!("{} shutting down", self.contract.display_name());
                    break;
                }
                result = self.process_batch() => {
                    if let Err(e) = result {
                        error!("❌ Error processing blocks ({}): {:?}", self.contract.display_name(), e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
            tokio::time::sleep(self.poll_interval).await;
        }

        Ok(())
    }

    async fn process_batch(&mut self) -> Result<()> {
        tokio::time::sleep(self.request_delay).await;

        let latest_block = with_retry(
            || async {
                tokio::time::timeout(
                    Duration::from_secs(30),
                    self.provider.get_block_number(),
                )
                .await
                .map_err(|_| Error::blockchain("get_block_number timeout after 30s"))?
                .map(|b| b.as_u64())
                .map_err(|e| Error::blockchain(format!("Failed to get block number: {}", e)))
            },
            self.max_retries,
            self.retry_delay,
            "get_block_number",
        )
        .await?;

        if latest_block <= self.current_block {
            return Ok(());
        }

        let to_block = std::cmp::min(self.current_block + self.batch_size, latest_block);

        let filter = Filter::new()
            .address(self.contract_address)
            .from_block(self.current_block)
            .to_block(to_block);

        tokio::time::sleep(self.request_delay).await;

        let logs = with_retry(
            || async {
                tokio::time::timeout(
                    Duration::from_secs(30),
                    self.provider.get_logs(&filter),
                )
                .await
                .map_err(|_| Error::blockchain("get_logs timeout after 30s"))?
                .map_err(|e| Error::blockchain(format!("Failed to get logs: {}", e)))
            },
            self.max_retries,
            self.retry_delay,
            "get_logs",
        )
        .await?;

        if !logs.is_empty() {
            info!(
                "🔍 Found {} logs in blocks {}-{} ({})",
                logs.len(),
                self.current_block,
                to_block,
                self.contract.display_name()
            );
        }

        // Fan out log processing concurrently — each log independently hits Kafka
        // (and no shared mutable state), so processing them in parallel gives
        // ~N× throughput on busy blocks without any coordination overhead.
        if !logs.is_empty() {
            let kafka = self.kafka.clone();
            let parse_type = self.contract.event_parse_type();
            let indexer_name = self.contract.display_name();
            let direct = self.direct.clone();

            let futs: Vec<_> = logs
                .into_iter()
                .map(|log| {
                    let kafka = kafka.clone();
                    let direct = direct.clone();
                    async move { Self::process_log_static(&log, parse_type, &kafka, direct.as_ref()).await }
                })
                .collect();

            for (i, result) in futures::future::join_all(futs).await.into_iter().enumerate() {
                if let Err(e) = result {
                    warn!("Failed to process log[{}] ({}): {:?}", i, indexer_name, e);
                }
            }
        }

        // +1: Ethereum filters are inclusive on both ends; advance past to_block
        // so the next batch does not re-fetch and re-publish to_block's events.
        self.current_block = to_block + 1;
        save_last_indexed_block(
            &self.pool,
            &format!("{:?}", self.contract_address),
            self.contract.cursor_type_name(),
            to_block,
        )
        .await?;

        Ok(())
    }

    #[allow(dead_code)]
    async fn process_log(&self, log: &Log) -> Result<()> {
        Self::process_log_static(log, self.contract.event_parse_type(), &self.kafka, self.direct.as_ref()).await
    }

    /// Static helper so concurrent log processing can share a cloned `KafkaProducer`
    /// without borrowing `self` across multiple futures simultaneously.
    async fn process_log_static(
        log: &Log,
        parse_type: &'static str,
        kafka: &KafkaProducer,
        direct: Option<&Arc<crate::event_processor::DirectHandlers>>,
    ) -> Result<()> {
        let parsed = crate::events::parse_log(log, parse_type)?;

        if let Some(handlers) = direct {
            // Kafka disabled — write preference signals directly into the rec DB.
            // Convert ParsedEvent → BlockchainEvent via JSON so the handlers can
            // use the same field-access patterns as the Kafka EventProcessor path.
            let data = parsed.data.as_ref().and_then(|d| serde_json::to_value(d).ok());
            let bc_event = crate::kafka::BlockchainEvent {
                event_type: parsed.event_type.clone(),
                contract_address: parsed.contract_address.clone(),
                contract_type: parsed.contract_type.clone(),
                block_number: parsed.block_number,
                transaction_hash: parsed.transaction_hash.clone(),
                log_index: parsed.log_index,
                timestamp: parsed.timestamp,
                data,
            };
            return handlers.dispatch(&bc_event).await.map_err(|e| {
                crate::error::Error::Internal { source: Some(e.into()) }
            });
        }

        let kafka_key = crate::events::event_kafka_key(&parsed);
        kafka.send_event(parsed.kafka_topic, &kafka_key, &parsed).await
    }
}

/// Common configuration for indexers
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct IndexerConfig {
    pub name: &'static str,
    pub contract_address: Address,
    pub poll_interval: Duration,
    pub batch_size: u64,
    pub max_retries: u32,
    pub retry_delay: Duration,
}

/// Indexer state stored in database
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct IndexerState {
    pub contract_address: String,
    pub contract_type: String,
    pub last_block: u64,
}

pub async fn get_last_indexed_block(
    pool: &PgPool,
    contract_address: &str,
    contract_type: &str,
) -> Result<Option<u64>> {
    let addr_lower = contract_address.to_lowercase();
    let result: Option<i64> = sqlx::query_scalar::<_, i64>(
        "SELECT last_block::bigint FROM indexer_state WHERE LOWER(contract_address) = $1 AND contract_type = $2",
    )
    .bind(addr_lower)
    .bind(contract_type)
    .fetch_optional(pool)
    .await?;

    Ok(result.map(|b| {
        u64::try_from(b).unwrap_or_else(|_| {
            tracing::error!(block = b, "indexer_state.last_block is negative — resetting to genesis");
            0u64
        })
    }))
}

/// Save last indexed block to database
///
/// Unique constraint is on (contract_address, contract_type) so multiple indexers
/// sharing a contract address each maintain their own cursor row without collision.
#[instrument(skip(pool))]
pub async fn save_last_indexed_block(
    pool: &PgPool,
    contract_address: &str,
    contract_type: &str,
    block: u64,
) -> Result<()> {
    let addr_lower = contract_address.to_lowercase();

    sqlx::query!(
        r#"
        INSERT INTO indexer_state (id, contract_address, contract_type, last_block, inserted_at, updated_at)
        VALUES (gen_random_uuid(), $1, $2, $3, NOW(), NOW())
        ON CONFLICT (contract_address, contract_type) DO UPDATE
        SET last_block = EXCLUDED.last_block,
            updated_at = NOW()
        "#,
        addr_lower,
        contract_type,
        block as i64
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Retry helper for RPC calls with exponential backoff
/// 
/// Handles rate limiting (429) with longer backoff periods
pub async fn with_retry<T, F, Fut>(
    operation: F,
    max_retries: u32,
    initial_delay: Duration,
    operation_name: &str,
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut delay = initial_delay;
    let mut last_error = None;

    for attempt in 0..max_retries {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                // Check if it's a rate limit error (429)
                let is_rate_limit = e.to_string().contains("429") || e.to_string().contains("Too Many Requests");
                
                if !e.is_retryable() && !is_rate_limit {
                    return Err(e);
                }

                if is_rate_limit {
                    warn!(
                        "⚠️  {} hit rate limit (attempt {}/{}): {:?}",
                        operation_name,
                        attempt + 1,
                        max_retries,
                        e
                    );
                    // Use much longer backoff for rate limits
                    let rate_limit_delay = Duration::from_secs(30 + (attempt as u64 * 10));
                    warn!("💤 Waiting {} seconds before retry due to rate limit...", rate_limit_delay.as_secs());
                    tokio::time::sleep(rate_limit_delay).await;
                } else {
                    warn!(
                        "{} failed (attempt {}/{}): {:?}",
                        operation_name,
                        attempt + 1,
                        max_retries,
                        e
                    );

                    last_error = Some(e);

                    if attempt + 1 < max_retries {
                        tokio::time::sleep(delay).await;
                        delay = std::cmp::min(delay * 2, Duration::from_secs(30));
                    }
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| Error::blockchain("Max retries exceeded")))
}

/// Parse Ethereum address from string
pub fn parse_address(addr: &str) -> Result<Address> {
    addr.parse().map_err(|_| Error::InvalidAddress {
        address: addr.to_string(),
    })
}

/// Format address for logging (truncated)
#[allow(dead_code)]
pub fn format_address(addr: &Address) -> String {
    let s = format!("{:?}", addr);
    if s.len() > 12 {
        format!("{}...{}", &s[..8], &s[s.len() - 4..])
    } else {
        s
    }
}

/// Decode uint256 from log data
#[allow(dead_code)]
pub fn decode_uint256(data: &[u8], offset: usize) -> Result<U256> {
    if data.len() < offset + 32 {
        return Err(Error::EventDecode {
            event: "uint256",
            message: "Insufficient data".into(),
        });
    }
    Ok(U256::from_big_endian(&data[offset..offset + 32]))
}

/// Decode address from log data
#[allow(dead_code)]
pub fn decode_address_from_data(data: &[u8], offset: usize) -> Result<Address> {
    if data.len() < offset + 32 {
        return Err(Error::EventDecode {
            event: "address",
            message: "Insufficient data".into(),
        });
    }
    // Address is in the last 20 bytes of the 32-byte word
    Ok(Address::from_slice(&data[offset + 12..offset + 32]))
}

/// Decode string from log data (dynamic type)
#[allow(dead_code)]
pub fn decode_string(data: &[u8], offset: usize) -> Result<String> {
    if data.len() < offset + 32 {
        return Err(Error::EventDecode {
            event: "string",
            message: "Insufficient data for string offset".into(),
        });
    }

    // Read offset to string data
    let string_offset = U256::from_big_endian(&data[offset..offset + 32]).as_usize();

    if data.len() < string_offset + 32 {
        return Err(Error::EventDecode {
            event: "string",
            message: "Insufficient data for string length".into(),
        });
    }

    // Read string length
    let length = U256::from_big_endian(&data[string_offset..string_offset + 32]).as_usize();

    if data.len() < string_offset + 32 + length {
        return Err(Error::EventDecode {
            event: "string",
            message: "Insufficient data for string content".into(),
        });
    }

    // Read string data
    let string_data = &data[string_offset + 32..string_offset + 32 + length];
    String::from_utf8(string_data.to_vec()).map_err(|_| Error::EventDecode {
        event: "string",
        message: "Invalid UTF-8 string".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_address() {
        let addr: Address = "0x1234567890123456789012345678901234567890"
            .parse()
            .unwrap();
        let formatted = format_address(&addr);
        assert!(formatted.contains("..."));
    }

    #[test]
    fn test_decode_uint256() {
        let mut data = vec![0u8; 32];
        data[31] = 42;
        let value = decode_uint256(&data, 0).unwrap();
        assert_eq!(value, U256::from(42));
    }
}
