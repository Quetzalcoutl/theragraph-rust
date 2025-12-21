//! TheraSocial unified contract indexer
//!
//! Indexes unified events (ContentMinted, ContentLiked, ContentCopyMinted, ContentCommented, ContentBlocked)

use crate::error::{Error, Result};
use crate::indexer::{get_last_indexed_block, parse_address, save_last_indexed_block, with_retry};
use crate::kafka::KafkaProducer;
use crate::AppState;
use ethers::prelude::*;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{error, info, instrument, warn};

struct TheraSocialIndexer {
    provider: Arc<Provider<Http>>,
    contract_address: Address,
    kafka: KafkaProducer,
    pool: PgPool,
    poll_interval: Duration,
    batch_size: u64,
    current_block: u64,
}

pub async fn run_with_state(state: Arc<AppState>) -> Result<()> {
    let contract_address = parse_address(state.config.contracts.thera_friends.as_str())?;
    let provider = Provider::<Http>::try_from(state.config.blockchain.rpc_url.as_str())
        .map_err(|e| Error::blockchain(format!("Failed to create provider: {}", e)))?;

    let start_block = get_last_indexed_block(state.db.pool(), &format!("{:?}", contract_address))
        .await?
        .unwrap_or(state.config.blockchain.start_block);

    let mut indexer = TheraSocialIndexer {
        provider: Arc::new(provider),
        contract_address,
        kafka: state.kafka.clone(),
        pool: state.db.pool().clone(),
        poll_interval: state.config.blockchain.poll_interval,
        batch_size: state.config.blockchain.batch_size,
        current_block: start_block,
    };

    let mut shutdown_rx = state.shutdown.subscribe();
    indexer.run(&mut shutdown_rx).await
}

impl TheraSocialIndexer {
    #[instrument(skip(self, shutdown_rx), fields(contract = %self.contract_address))]
    async fn run(&mut self, shutdown_rx: &mut broadcast::Receiver<()>) -> Result<()> {
        info!("🧩 TheraSocialIndexer started for contract: {:?}", self.contract_address);
        info!("📍 Starting from block: {}", self.current_block);

        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => {
                    info!("TheraSocialIndexer shutting down");
                    break;
                }
                result = self.process_batch() => {
                    if let Err(e) = result {
                        error!("❌ Error processing TheraSocial blocks: {:?}", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }

            tokio::time::sleep(self.poll_interval).await;
        }

        Ok(())
    }

    async fn process_batch(&mut self) -> Result<()> {
        let latest_block = with_retry(
            || async {
                self.provider
                    .get_block_number()
                    .await
                    .map(|b| b.as_u64())
                    .map_err(|e| Error::blockchain(format!("Failed to get block number: {}", e)))
            },
            3,
            Duration::from_millis(500),
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

        let logs = with_retry(
            || async {
                self.provider
                    .get_logs(&filter)
                    .await
                    .map_err(|e| Error::blockchain(format!("Failed to get logs: {}", e)))
            },
            3,
            Duration::from_millis(500),
            "get_logs",
        )
        .await?;

        if !logs.is_empty() {
            info!("🔍 Found {} logs in blocks {}-{}", logs.len(), self.current_block, to_block);
        }

        for log in logs {
            if let Err(e) = self.process_log(&log).await {
                warn!("Failed to process log: {:?}", e);
            }
        }

        self.current_block = to_block;
        save_last_indexed_block(
            &self.pool,
            &format!("{:?}", self.contract_address),
            "friends",
            to_block,
        )
        .await?;

        Ok(())
    }

    async fn process_log(&self, log: &Log) -> Result<()> {
        let parsed = crate::events::parse_log(log, "friends")?;
        let kafka_key = crate::events::event_kafka_key(&parsed);
        let topic = if parsed.event_type == "Unknown" { "blockchain.events" } else { "user.actions" };

        self.kafka.send_event(topic, &kafka_key, &parsed).await
    }
}
