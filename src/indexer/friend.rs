//! TheraFriends Contract Indexer
//!
//! Indexes social graph events from the TheraFriends smart contract.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::indexer::{get_last_indexed_block, parse_address, save_last_indexed_block, with_retry};
use crate::kafka::{BlockchainEvent, KafkaProducer};
use crate::AppState;
use ethers::prelude::*;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{error, info, instrument, warn};

/// Friend indexer state
struct FriendIndexer {
    provider: Arc<Provider<Http>>,
    contract_address: Address,
    kafka: KafkaProducer,
    pool: PgPool,
    poll_interval: Duration,
    batch_size: u64,
    current_block: u64,
}

/// Run the friend indexer with AppState
pub async fn run_with_state(state: Arc<AppState>) -> Result<()> {
    let contract_address = parse_address(state.config.contracts.thera_friends.as_str())?;
    let provider = Provider::<Http>::try_from(state.config.blockchain.rpc_url.as_str())
        .map_err(|e| Error::blockchain(format!("Failed to create provider: {}", e)))?;

    let start_block = get_last_indexed_block(state.db.pool(), &format!("{:?}", contract_address))
        .await?
        .unwrap_or(state.config.blockchain.start_block);

    let mut indexer = FriendIndexer {
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

impl FriendIndexer {
    #[instrument(skip(self, shutdown_rx), fields(contract = %self.contract_address))]
    async fn run(&mut self, shutdown_rx: &mut broadcast::Receiver<()>) -> Result<()> {
        info!(
            "👥 FriendIndexer started for contract: {:?}",
            self.contract_address
        );
        info!("📍 Starting from block: {}", self.current_block);

        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => {
                    info!("👥 FriendIndexer shutting down");
                    break;
                }
                result = self.process_batch() => {
                    if let Err(e) = result {
                        error!("❌ Error processing blocks: {:?}", e);
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
            info!(
                "🔍 Found {} logs in blocks {}-{}",
                logs.len(),
                self.current_block,
                to_block
            );
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
            "friend",
            to_block,
        )
        .await?;

        Ok(())
    }

    async fn process_log(&self, log: &Log) -> Result<()> {
        // Parse the log using the events module for proper event type detection
        let parsed_event = crate::events::parse_log(log, "friends")?;
        let kafka_key = crate::events::event_kafka_key(&parsed_event);
        let topic = if parsed_event.event_type == "Unknown" {
            "blockchain.events"
        } else {
            "user.actions"
        };

        self.kafka
            .send_event(topic, &kafka_key, &parsed_event)
            .await
    }
}

/// Legacy run function for backwards compatibility
#[allow(dead_code)]
pub async fn run(config: Config, kafka_producer: KafkaProducer, db_pool: PgPool) -> Result<()> {
    info!(
        "👥 FriendIndexer started for contract: {}",
        config.contracts.thera_friends
    );

    let provider = Provider::<Http>::try_from(config.blockchain.rpc_url.as_str())
        .map_err(|e| Error::blockchain(format!("Failed to create provider: {}", e)))?;
    let provider = Arc::new(provider);

    let contract_address = parse_address(&config.contracts.thera_friends)?;

    let mut current_block = get_last_indexed_block(&db_pool, &format!("{:?}", contract_address))
        .await?
        .unwrap_or(config.blockchain.start_block);

    info!("📍 Starting from block: {}", current_block);

    loop {
        match process_blocks(
            &provider,
            &kafka_producer,
            &db_pool,
            contract_address,
            &mut current_block,
            config.blockchain.batch_size,
        )
        .await
        {
            Ok(_) => {}
            Err(e) => {
                error!("❌ Error processing blocks: {:?}", e);
            }
        }

        tokio::time::sleep(config.blockchain.poll_interval).await;
    }
}

#[allow(dead_code)]
async fn process_blocks(
    provider: &Arc<Provider<Http>>,
    kafka_producer: &KafkaProducer,
    db_pool: &PgPool,
    contract_address: Address,
    current_block: &mut u64,
    batch_size: u64,
) -> Result<()> {
    let latest_block = provider
        .get_block_number()
        .await
        .map_err(|e| Error::blockchain(format!("Failed to get block number: {}", e)))?
        .as_u64();

    if latest_block <= *current_block {
        return Ok(());
    }

    let to_block = std::cmp::min(*current_block + batch_size, latest_block);

    let filter = Filter::new()
        .address(contract_address)
        .from_block(*current_block)
        .to_block(to_block);

    let logs = provider
        .get_logs(&filter)
        .await
        .map_err(|e| Error::blockchain(format!("Failed to get logs: {}", e)))?;

    if !logs.is_empty() {
        info!(
            "�� Found {} logs in blocks {}-{}",
            logs.len(),
            current_block,
            to_block
        );
    }

    for log in logs {
        let block_number = log.block_number.map(|b| b.as_u64()).unwrap_or(0);
        let tx_hash = format!("{:?}", log.transaction_hash.unwrap_or_default());

        let event = BlockchainEvent::new(
            "FriendEvent",
            format!("{:?}", log.address),
            "friend",
            block_number,
            tx_hash,
        );

        if let Err(e) = kafka_producer
            .send_event("user.actions", "friend.event", &event)
            .await
        {
            warn!("Failed to send event: {:?}", e);
        }
    }

    *current_block = to_block;
    save_last_indexed_block(db_pool, &format!("{:?}", contract_address), "friend", to_block).await?;

    Ok(())
}
