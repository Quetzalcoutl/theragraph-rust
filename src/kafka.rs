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

// ============================================================================
// Transport abstraction — swap real Kafka for a mock in tests
// ============================================================================

/// One-message delivery seam above rdkafka.
///
/// A second adapter (MockTransport) lives in the test module below; that makes
/// this a real seam — not a hypothetical one.
pub trait SendTransport: Send + Sync {
    fn deliver<'a>(
        &'a self,
        topic: &'a str,
        key: &'a str,
        payload: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<(i32, i64)>> + Send + 'a>>;
}

/// Production adapter: wraps `rdkafka::FutureProducer`.
pub struct RdkafkaTransport {
    producer: Arc<FutureProducer>,
    delivery_timeout: Duration,
}

impl SendTransport for RdkafkaTransport {
    fn deliver<'a>(
        &'a self,
        topic: &'a str,
        key: &'a str,
        payload: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<(i32, i64)>> + Send + 'a>> {
        Box::pin(async move {
            let record = FutureRecord::to(topic).key(key).payload(payload);
            self.producer
                .send(record, Timeout::After(self.delivery_timeout))
                .await
                .map_err(|(e, _)| crate::error::Error::Kafka {
                    message: e.to_string().into(),
                    source: Some(e),
                })
        })
    }
}

/// Determines how send_event backs off between attempts.
///
/// Extracted from the inline retry loop so callers (tests) can inject
/// zero-delay policies without sleeping real time.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_backoff_ms: u64,
}

impl RetryPolicy {
    pub fn new(max_attempts: u32, base_backoff_ms: u64) -> Self {
        Self { max_attempts, base_backoff_ms }
    }

    /// Delay before retrying attempt `n` (1-based). Returns `None` when retries are exhausted.
    pub fn backoff_for(&self, attempt: u32) -> Option<Duration> {
        if attempt >= self.max_attempts {
            return None;
        }
        let exp = 2u64.saturating_pow(attempt.saturating_sub(1));
        let base = self.base_backoff_ms.saturating_mul(exp);
        let jitter = rand::Rng::gen_range(&mut rand::thread_rng(), 0u64..100);
        Some(Duration::from_millis(base.saturating_add(jitter)))
    }
}

/// No-op adapter: discards all messages. Used by `KafkaProducer::noop()`.
struct NoopTransport;

impl SendTransport for NoopTransport {
    fn deliver<'a>(
        &'a self,
        _topic: &'a str,
        _key: &'a str,
        _payload: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<(i32, i64)>> + Send + 'a>> {
        Box::pin(async { Ok((0, 0)) })
    }
}

/// Kafka producer with batching and reliability features
#[derive(Clone)]
pub struct KafkaProducer {
    /// None when Kafka is disabled — no rdkafka client or background threads created.
    producer: Option<Arc<FutureProducer>>,
    /// Delivery seam — send path goes through this transport.
    transport: Arc<dyn SendTransport + Send + Sync>,
    config: Arc<KafkaProducerMetrics>,
    enabled: bool,
    /// How long we'll wait for a send to complete before timing out
    #[allow(dead_code)]
    delivery_timeout: Duration,
    /// send retry behavior
    send_max_attempts: u32,
    send_backoff_base_ms: u64,
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
    #[allow(dead_code)]
    fn config_send_max_attempts(&self) -> u32 { self.send_max_attempts }
    #[allow(dead_code)]
    fn config_send_backoff_base_ms(&self) -> u64 { self.send_backoff_base_ms }

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

        let producer_arc = Arc::new(producer);
        let transport = Arc::new(RdkafkaTransport {
            producer: Arc::clone(&producer_arc),
            delivery_timeout: config.producer.delivery_timeout,
        });

        info!("Kafka producer created successfully");

        Ok(Self {
            producer: Some(producer_arc),
            transport,
            config: Arc::new(KafkaProducerMetrics::new()),
            enabled: true,
            delivery_timeout: config.producer.delivery_timeout,
            send_max_attempts: config.producer.send_max_attempts,
            send_backoff_base_ms: config.producer.send_backoff_base_ms,
        })
    }

    /// Create a no-op producer (when Kafka is disabled)
    pub fn noop() -> Self {
        Self {
            // No rdkafka client at all — avoids background connect threads.
            producer: None,
            transport: Arc::new(NoopTransport),
            config: Arc::new(KafkaProducerMetrics::new()),
            enabled: false,
            delivery_timeout: Duration::from_secs(5),
            send_max_attempts: 1,
            send_backoff_base_ms: 200,
        }
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
                notifications_priority: "notifications.priority".to_string(),
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
                send_max_attempts: 5,
                send_backoff_base_ms: 200,
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

        let policy = RetryPolicy::new(self.send_max_attempts, self.send_backoff_base_ms);

        for attempt in 1..=policy.max_attempts {
            match self.transport.deliver(topic, key, payload.as_str()).await {
                Ok((partition, offset)) => {
                    debug!("Delivered to partition {partition} offset {offset} (attempt {attempt}/{})", policy.max_attempts);
                    self.config.messages_sent.fetch_add(1, Ordering::Relaxed);
                    self.config.bytes_sent.fetch_add(payload_len as u64, Ordering::Relaxed);
                    return Ok(());
                }
                Err(err) => {
                    self.config.messages_failed.fetch_add(1, Ordering::Relaxed);
                    error!("Attempt {attempt}/{} failed: {:?}", policy.max_attempts, err);

                    match policy.backoff_for(attempt) {
                        None => {
                            if let Some(producer) = self.producer.as_ref() {
                                match producer.client().fetch_metadata(None, Timeout::After(Duration::from_secs(5))) {
                                    Ok(md) => {
                                        let brokers: Vec<String> = md.brokers().iter().map(|b| format!("{}:{}", b.host(), b.port())).collect();
                                        error!("Broker metadata on failure: brokers={brokers:?}, topics={}", md.topics().len());
                                    }
                                    Err(merr) => error!("Failed to fetch broker metadata: {merr:?}"),
                                }
                            }
                            // Structured DLQ log — full payload preserved for manual replay.
                            // During broker recovery: grep 'kafka.dlq=true' | jq to extract
                            // all lost events and re-send them via the replay utility.
                            error!(
                                kafka.dlq           = true,
                                kafka.topic         = topic,
                                kafka.key           = key,
                                kafka.payload       = %payload,
                                kafka.attempts      = policy.max_attempts,
                                kafka.final_error   = %err,
                                "kafka_dlq: event undeliverable after {} attempts — logged for manual replay",
                                policy.max_attempts
                            );
                            return Err(err);
                        }
                        Some(delay) => {
                            debug!("Backing off {}ms before attempt {}/{}", delay.as_millis(), attempt + 1, policy.max_attempts);
                            tokio::time::sleep(delay).await;
                        }
                    }
                }
            }
        }

        // S30-12: unreachable! panics when max_attempts=0 (no iterations occur).
        // Return a descriptive error instead so callers get a Result, not a panic.
        Err(Error::KafkaProducerFailed { retries: 0 })
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
        let mut payloads: Vec<(String, String)> = Vec::with_capacity(events.len());
        for (key, event) in events {
            let payload = serde_json::to_string(event).map_err(|e| {
                crate::error::Error::InvalidFormat {
                    message: format!("Failed to serialize event for key '{}': {}", key, e).into(),
                }
            })?;
            payloads.push((key.clone(), payload));
        }

        let policy = RetryPolicy::new(self.send_max_attempts, self.send_backoff_base_ms);
        let mut errors: Vec<crate::error::Error> = Vec::with_capacity(payloads.len());

        for (key, payload) in &payloads {
            // TAG-S28-06: per-message retry with exponential backoff before DLQ log.
            // send_event already retries at the transport layer, but send_batch had no
            // retry sweep — a transient Kafka leader rebalance would permanently drop
            // the whole batch.  Uses configured RetryPolicy instead of hardcoded 3 attempts.
            let mut last_err: Option<crate::error::Error> = None;
            for attempt in 1u32..=policy.max_attempts {
                match self.transport.deliver(topic, key.as_str(), payload.as_str()).await {
                    Ok(_) => { last_err = None; break; }
                    Err(e) => {
                        if let Some(delay) = policy.backoff_for(attempt) {
                            debug!("send_batch retry {}/{} key='{}': {:?}", attempt, policy.max_attempts, key, e);
                            tokio::time::sleep(delay).await;
                        }
                        last_err = Some(e);
                    }
                }
            }
            if let Some(e) = last_err {
                // Structured DLQ log — full payload preserved for manual replay.
                error!(
                    "[send_batch DLQ] permanent failure key='{}' topic='{}' payload='{}': {:?}",
                    key, topic, payload, e
                );
                errors.push(e);
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
            Err(errors.into_iter().next().unwrap_or_else(|| {
                // SAFETY: we only enter this branch when errors is non-empty;
                // the is_empty() check above guarantees at least one element.
                unreachable!("errors non-empty but iterator yielded None")
            }))
        }
    }

    /// Flush pending messages
    pub fn flush(&self, timeout: Duration) {
        if !self.enabled {
            return;
        }

        info!("Flushing Kafka producer...");
        let Some(producer) = self.producer.as_ref() else { return; };
        match producer.flush(Timeout::After(timeout)) {
            Ok(()) => info!("Kafka producer flushed"),
            Err(e) => error!("Kafka flush failed — messages may be lost: {:?}", e),
        }
    }

    /// Get producer statistics
    pub fn stats(&self) -> ProducerStats {
        ProducerStats {
            messages_sent: self.config.messages_sent.load(Ordering::Relaxed),
            messages_failed: self.config.messages_failed.load(Ordering::Relaxed),
            bytes_sent: self.config.bytes_sent.load(Ordering::Relaxed),
            in_flight: self.producer.as_ref().map(|p| p.in_flight_count() as u64).unwrap_or(0),
        }
    }

    /// Check if producer is healthy
    pub fn is_healthy(&self) -> bool {
        if !self.enabled {
            return true;
        }
        // Check if we can reach the broker
        self.producer.as_ref().map_or(false, |p| p.in_flight_count() < 10000)
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
        if self.enabled {
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

    /// Test double: pre-queued responses, records calls for assertion.
    pub struct MockTransport {
        /// Each call pops the front response. Panics if empty (misconfigured test).
        pub responses: std::sync::Mutex<std::collections::VecDeque<crate::error::Result<(i32, i64)>>>,
        pub calls: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    impl MockTransport {
        pub fn succeeding(count: usize) -> Self {
            Self {
                responses: std::sync::Mutex::new(
                    (0..count).map(|i| Ok((0i32, i as i64))).collect(),
                ),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        pub fn failing(count: usize, msg: &str) -> Self {
            let msg = msg.to_string();
            Self {
                responses: std::sync::Mutex::new(
                    (0..count)
                        .map(|_| {
                            Err(crate::error::Error::Internal {
                                source: Some(anyhow::anyhow!("{}", msg.clone()).into()),
                            })
                        })
                        .collect(),
                ),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl SendTransport for MockTransport {
        fn deliver<'a>(
            &'a self,
            topic: &'a str,
            key: &'a str,
            payload: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<(i32, i64)>> + Send + 'a>> {
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockTransport: no more queued responses");
            self.calls.lock().unwrap().push((
                topic.to_string(),
                key.to_string(),
                payload.to_string(),
            ));
            Box::pin(async move { response })
        }
    }

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
