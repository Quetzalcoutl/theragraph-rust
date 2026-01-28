//! Blockchain Indexers
//!
//! Each indexer polls a specific smart contract for events and processes them.
//! Events are published to Kafka for downstream consumers.
//!
//! # Indexers
//!
//! - `friend` - TheraFriends contract (social features)
//! - `thera_friends` - TheraFriends unified contract (all content types)
//! - `thera_social` - TheraFriends unified contract (social features)

pub mod friend;
pub mod thera_friends;
pub mod thera_social;

use crate::error::{Error, Result};
use ethers::prelude::*;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{instrument, warn};

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
        "SELECT last_block FROM indexer_state WHERE LOWER(contract_address) = $1 AND contract_type = $2",
    )
    .bind(addr_lower)
    .bind(contract_type)
    .fetch_optional(pool)
    .await?;

    Ok(result.map(|b| b as u64))
}

/// Save last indexed block to database
///
/// Uses case-insensitive upsert to handle both checksummed and lowercase addresses.
/// Addresses are stored in lowercase for consistency.
/// Note: The database has a unique constraint on contract_address alone (not contract_address + contract_type)
#[instrument(skip(pool))]
pub async fn save_last_indexed_block(
    pool: &PgPool,
    contract_address: &str,
    contract_type: &str,
    block: u64,
) -> Result<()> {
    let addr_lower = contract_address.to_lowercase();

    // Use ON CONFLICT with the actual constraint (contract_address only)
    // Update both last_block and contract_type to handle type changes
    sqlx::query!(
        r#"
        INSERT INTO indexer_state (id, contract_address, contract_type, last_block, inserted_at, updated_at) 
        VALUES (gen_random_uuid(), $1, $2, $3, NOW(), NOW())
        ON CONFLICT (contract_address) DO UPDATE 
        SET last_block = EXCLUDED.last_block, 
            contract_type = EXCLUDED.contract_type,
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
                if !e.is_retryable() {
                    return Err(e);
                }

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
