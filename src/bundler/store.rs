// ─── Receipt Store ────────────────────────────────────────────────────────────
//
// Two backends:
//   1. Redis  — production / multi-replica deployments, auto-expires keys.
//   2. DashMap — zero-dependency fallback for single-replica deployments.

use alloy::primitives::B256;
use dashmap::DashMap;
use eyre::{Result, WrapErr};
use redis::{aio::ConnectionManager, AsyncCommands};
use std::{sync::Arc, time::{SystemTime, UNIX_EPOCH}};
use tracing::{debug, warn};

use super::types::Receipt;

const TTL_SECS: u64 = 86_400;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

// Memory entry pairs receipt JSON with insertion timestamp for TTL enforcement.
// Box<str> not String for both fields: entries are inserted once (via
// `.insert()`, never mutated in place) and self-prune on every write via
// `.retain()` — the map holds up to 24h of receipts, so the 8-byte capacity
// field String carries on both the key and the JSON value is real, ongoing
// waste for however many receipts accumulate in that window.
type MemEntry = (Box<str>, u64); // (json, inserted_at_secs)

enum Backend {
    Redis(ConnectionManager),
    Memory(Arc<DashMap<Box<str>, MemEntry>>),
}

/// Thread-safe, `Clone`-cheap receipt store.
#[derive(Clone)]
pub struct ReceiptStore {
    inner: Arc<Backend>,
}

impl ReceiptStore {
    pub async fn redis(url: &str) -> Result<Self> {
        let client = redis::Client::open(url).wrap_err("Invalid Redis URL")?;
        let mgr = ConnectionManager::new(client)
            .await
            .wrap_err("Cannot connect to Redis")?;
        tracing::info!("Receipt store backend: Redis at {url}");
        Ok(Self { inner: Arc::new(Backend::Redis(mgr)) })
    }

    pub fn memory() -> Self {
        tracing::info!("Receipt store backend: in-memory DashMap (set REDIS_URL for persistence)");
        Self { inner: Arc::new(Backend::Memory(Arc::new(DashMap::new()))) }
    }

    pub async fn from_config(redis_url: Option<&str>) -> Result<Self> {
        match redis_url {
            Some(url) => Self::redis(url).await,
            None      => Ok(Self::memory()),
        }
    }

    pub async fn set(&self, user_op_hash: B256, receipt: &Receipt) {
        let key = format!("receipt:{:#x}", user_op_hash);
        let value = match serde_json::to_string(receipt) {
            Ok(v) => v,
            Err(e) => { warn!("Failed to serialise receipt: {e}"); return; }
        };

        match self.inner.as_ref() {
            Backend::Redis(mgr) => {
                let mut conn = mgr.clone();
                let _: Result<(), _> = conn
                    .set_ex(&key, &value, TTL_SECS)
                    .await
                    .map_err(|e| warn!("Redis SET failed: {e}"));
            }
            Backend::Memory(map) => {
                // Evict expired entries on write to keep map bounded
                map.retain(|_, (_, inserted)| now_secs().saturating_sub(*inserted) < TTL_SECS);
                map.insert(key.into_boxed_str(), (value.into_boxed_str(), now_secs()));
            }
        }
        debug!("Stored receipt for {:#x}", user_op_hash);
    }

    pub async fn get(&self, user_op_hash: B256) -> Option<Receipt> {
        let key = format!("receipt:{:#x}", user_op_hash);

        let raw: Option<String> = match self.inner.as_ref() {
            Backend::Redis(mgr) => {
                let mut conn = mgr.clone();
                conn.get(&key).await
                    .map_err(|e| warn!("ReceiptStore Redis GET failed for {key}: {e}"))
                    .unwrap_or(None)
            }
            Backend::Memory(map) => {
                map.get(key.as_str()).and_then(|r| {
                    let (json, inserted) = r.value();
                    if now_secs().saturating_sub(*inserted) < TTL_SECS {
                        Some(json.as_ref().to_owned())
                    } else {
                        None // expired — will be pruned on next write
                    }
                })
            }
        };

        raw.and_then(|s| serde_json::from_str(&s).ok())
    }
}
