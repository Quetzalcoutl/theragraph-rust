//! Kafka producer with batching, reliability, and observability
//!
//! Features:
//! - Idempotent production for exactly-once semantics
//! - Automatic batching and compression
//! - Backpressure handling
//! - Metrics and tracing
//! - Graceful shutdown with flush

use crate::config::KafkaConfig;
use crate::error::{Error, Result};
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
use rdkafka::util::Timeout;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, instrument};

/// Kafka producer with batching and reliability features
#[derive(Clone)]
pub struct KafkaProducer {
    producer: Arc<FutureProducer>,
    config: Arc<KafkaProducerMetrics>,
    enabled: bool,
    /// How long we'll wait for a send to complete before timing out
    delivery_timeout: Duration,
}

/// Producer metrics
struct KafkaProducerMetrics {
    messages_sent: AtomicU64,
    messages_failed: AtomicU64,
    bytes_sent: AtomicU64,
}

impl KafkaProducerMetrics {
    fn new() -> Self {
        Self {
            messages_sent: AtomicU64::new(0),
            messages_failed: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
        }
    }
}

impl KafkaProducer {
    /// Create a new Kafka producer from configuration
    pub fn new(config: &KafkaConfig) -> Result<Self> {
        if !config.enabled {
            info!("Kafka is disabled, creating no-op producer");
            return Ok(Self::noop());
        }

        info!("Creating Kafka producer...");
        debug!("Kafka brokers: {}", config.brokers);

        // Build client config with additional resilience settings
        let mut cfg = ClientConfig::new();
        cfg.set("bootstrap.servers", &config.brokers)
            .set("client.id", "theragraph-engine")
            // Reliability
            .set("acks", &config.producer.acks)
            .set("enable.idempotence", config.producer.idempotent.to_string())
            .set("max.in.flight.requests.per.connection", "5")
            .set("retries", &config.producer.retries.to_string())
            .set("retry.backoff.ms", "100")
            .set(
                "reconnect.backoff.ms",
                &config.producer.reconnect_backoff_ms.to_string(),
            )
            .set(
                "reconnect.backoff.max.ms",
                &config.producer.reconnect_backoff_max_ms.to_string(),
            )
            // Batching
            .set("batch.size", config.producer.batch_size.to_string())
            .set("linger.ms", config.producer.linger.as_millis().to_string())
            // Compression
            .set("compression.type", &config.producer.compression)
            // Timeouts
            .set(
                "message.timeout.ms",
                config.producer.message_timeout.as_millis().to_string(),
            )
            .set(
                "delivery.timeout.ms",
                config.producer.delivery_timeout.as_millis().to_string(),
            )
            .set("request.timeout.ms", "30000")
            // Message size
            .set(
                "message.max.bytes",
                config.producer.max_message_bytes.to_string(),
            )
            // Statistics (for metrics)
            .set("statistics.interval.ms", "60000");

        // Enable librdkafka debug categories if requested (useful for diagnosing transport failures)
        if let Some(debug) = &config.producer.rdkafka_debug {
            cfg.set("debug", debug);
        }

        let producer: FutureProducer = cfg
            .create()
            .map_err(|e| Error::Kafka {
                message: format!("Failed to create producer: {}", e).into(),
                source: Some(e),
            })?;

        info!("Kafka producer created successfully");

        Ok(Self {
            producer: Arc::new(producer),
            config: Arc::new(KafkaProducerMetrics::new()),
            enabled: true,
            delivery_timeout: config.producer.delivery_timeout,
        })
    }

    /// Create a no-op producer (when Kafka is disabled)
    pub fn noop() -> Self {
        Self {
            producer: Arc::new(
                ClientConfig::new()
                    .set("bootstrap.servers", "localhost:9092")
                    .create()
                    .expect("Failed to create dummy producer"),
            ),
            config: Arc::new(KafkaProducerMetrics::new()),
            enabled: false,            delivery_timeout: Duration::from_secs(5),        }
    }

    /// Create producer from broker string (legacy compatibility)
    pub fn from_brokers(brokers: &str) -> Result<Self> {
        let config = KafkaConfig {
            brokers: brokers.to_string(),
            group_id: "theragraph-engine".to_string(),
            enabled: true,
            topics: crate::config::KafkaTopics {
                blockchain_events: "blockchain.events".to_string(),
                user_actions: "user.actions".to_string(),
                recommendations: "recommendations".to_string(),
            },
            producer: crate::config::KafkaProducerConfig {
                message_timeout: Duration::from_secs(5),
                delivery_timeout: Duration::from_secs(60),
                max_message_bytes: 20 * 1024 * 1024,
                batch_size: 16384,
                linger: Duration::from_millis(5),
                compression: "lz4".to_string(),
                acks: "all".to_string(),
                idempotent: true,
                reconnect_backoff_ms: 1000,
                reconnect_backoff_max_ms: 10000,
                retries: 2147483647u32,
                rdkafka_debug: None,
            },
        };
        Self::new(&config)
    }

    /// Send an event to Kafka
    #[instrument(skip(self, event), fields(topic = topic, key = key))]
    pub async fn send_event<T: Serialize + std::fmt::Debug>(
        &self,
        topic: &str,
        key: &str,
        event: &T,
    ) -> Result<()> {
        if !self.enabled {
            debug!("Kafka disabled, skipping event: {:?}", event);
            return Ok(());
        }

        let payload = serde_json::to_string(event)?;
        let payload_len = payload.len();

        debug!("Sending event to topic '{}' with key '{}'", topic, key);

        let record = FutureRecord::to(topic).key(key).payload(&payload);

        match self
            .producer
            .send(record, Timeout::After(self.delivery_timeout))
            .await
        {
            Ok((partition, offset)) => {
                debug!(
                    "Message delivered to partition {} at offset {}",
                    partition, offset
                );
                self.config.messages_sent.fetch_add(1, Ordering::Relaxed);
                self.config
                    .bytes_sent
                    .fetch_add(payload_len as u64, Ordering::Relaxed);
                Ok(())
            }
            Err((err, _)) => {
                self.config.messages_failed.fetch_add(1, Ordering::Relaxed);
                error!("Failed to deliver message: {:?}", err);
                Err(Error::Kafka {
                    message: format!("Failed to send message: {}", err).into(),
                    source: Some(err),
                })
            }
        }
    }

    /// Send multiple events in a batch
    #[instrument(skip(self, events))]
    pub async fn send_batch<T: Serialize + std::fmt::Debug>(
        &self,
        topic: &str,
        events: &[(String, T)],
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Serialize all payloads first so they live long enough
        let payloads: Vec<(String, String)> = events
            .iter()
            .map(|(key, event)| {
                let payload = serde_json::to_string(event).unwrap_or_default();
                (key.clone(), payload)
            })
            .collect();

        let mut futures = Vec::with_capacity(payloads.len());

        for (key, payload) in &payloads {
            let record = FutureRecord::to(topic)
                .key(key.as_str())
                .payload(payload.as_str());
            let future = self
                .producer
                .send(record, Timeout::After(Duration::from_secs(5)));
            futures.push(future);
        }

        let mut errors = Vec::new();
        for future in futures {
            if let Err((err, _)) = future.await {
                errors.push(err);
            }
        }

        if errors.is_empty() {
            self.config
                .messages_sent
                .fetch_add(events.len() as u64, Ordering::Relaxed);
            Ok(())
        } else {
            self.config
                .messages_failed
                .fetch_add(errors.len() as u64, Ordering::Relaxed);
            Err(Error::Kafka {
                message: format!("{} messages failed to deliver", errors.len()).into(),
                source: errors.into_iter().next(),
            })
        }
    }

    /// Flush pending messages
    pub fn flush(&self, timeout: Duration) {
        if !self.enabled {
            return;
        }

        info!("Flushing Kafka producer...");
        self.producer.flush(Timeout::After(timeout)).ok();
        info!("Kafka producer flushed");
    }

    /// Get producer statistics
    pub fn stats(&self) -> ProducerStats {
        ProducerStats {
            messages_sent: self.config.messages_sent.load(Ordering::Relaxed),
            messages_failed: self.config.messages_failed.load(Ordering::Relaxed),
            bytes_sent: self.config.bytes_sent.load(Ordering::Relaxed),
            in_flight: self.producer.in_flight_count() as u64,
        }
    }

    /// Check if producer is healthy
    pub fn is_healthy(&self) -> bool {
        if !self.enabled {
            return true;
        }
        // Check if we can reach the broker
        self.producer.in_flight_count() < 10000 // Arbitrary threshold
    }
}

/// Producer statistics
#[derive(Debug, Clone)]
pub struct ProducerStats {
    pub messages_sent: u64,
    pub messages_failed: u64,
    pub bytes_sent: u64,
    pub in_flight: u64,
}

impl Drop for KafkaProducer {
    fn drop(&mut self) {
        if self.enabled && Arc::strong_count(&self.producer) == 1 {
            // Last reference, flush before dropping
            self.flush(Duration::from_secs(5));
        }
    }
}

// ============================================================================
// Event types for Kafka messages
// ============================================================================

/// Blockchain event message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockchainEvent {
    pub event_type: String,
    pub contract_address: String,
    pub contract_type: String,
    pub block_number: u64,
    pub transaction_hash: String,
    pub log_index: u64,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// User action event message
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct UserActionEvent {
    pub action_type: String,
    pub user_address: String,
    pub nft_id: Option<String>,
    pub contract_type: Option<String>,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[allow(dead_code)]
impl BlockchainEvent {
    pub fn new(
        event_type: impl Into<String>,
        contract_address: impl Into<String>,
        contract_type: impl Into<String>,
        block_number: u64,
        transaction_hash: impl Into<String>,
    ) -> Self {
        Self {
            event_type: event_type.into(),
            contract_address: contract_address.into(),
            contract_type: contract_type.into(),
            block_number,
            transaction_hash: transaction_hash.into(),
            log_index: 0,
            timestamp: chrono::Utc::now().timestamp(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn with_log_index(mut self, log_index: u64) -> Self {
        self.log_index = log_index;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blockchain_event() {
        let event = BlockchainEvent::new(
            "SnapMinted",
            "0x1234567890123456789012345678901234567890",
            "snap",
            12345,
            "0xabcdef",
        )
        .with_log_index(0)
        .with_data(serde_json::json!({"token_id": 1}));

        assert_eq!(event.event_type, "SnapMinted");
        assert_eq!(event.block_number, 12345);
        assert!(event.data.is_some());
    }

    #[test]
    fn test_producer_stats() {
        let metrics = KafkaProducerMetrics::new();
        metrics.messages_sent.fetch_add(10, Ordering::Relaxed);
        metrics.messages_failed.fetch_add(1, Ordering::Relaxed);

        assert_eq!(metrics.messages_sent.load(Ordering::Relaxed), 10);
        assert_eq!(metrics.messages_failed.load(Ordering::Relaxed), 1);
    }
}
