//! TheraSocial unified contract indexer
//!
//! Indexes unified events (ContentMinted, ContentLiked, ContentCopyMinted, ContentCommented, ContentBlocked)

use crate::error::{Error, Result};
use crate::indexer::{get_last_indexed_block, parse_address, ContractType, GenericIndexer};
use crate::AppState;
use ethers::prelude::*;
use std::sync::Arc;
use std::time::Duration;

/// `ContractType` adapter for the TheraSocial (unified events) contract.
pub struct TheraSocialContract;

impl ContractType for TheraSocialContract {
    fn cursor_type_name(&self) -> &'static str { "friends" }
    fn event_parse_type(&self) -> &'static str { "friends" }
    fn display_name(&self) -> &'static str { "TheraSocialIndexer" }
}

pub async fn run_with_state(state: Arc<AppState>) -> Result<()> {
    let contract_address = parse_address(state.config.contracts.thera_friendz.as_str())?;
    let provider = Provider::<Http>::try_from(state.config.blockchain.rpc_url.as_str())
        .map_err(|e| Error::blockchain(format!("Failed to create provider: {}", e)))?;

    let start_block = get_last_indexed_block(
        state.db.pool(),
        &format!("{:?}", contract_address),
        "friends",
    )
    .await?
    // DB stores last-processed block; +1 so we start from the next unprocessed block.
    .map(|b| b + 1)
    .unwrap_or(state.config.blockchain.start_block);

    let mut indexer = GenericIndexer {
        contract: TheraSocialContract,
        provider: Arc::new(provider),
        contract_address,
        kafka: state.kafka.clone(),
        pool: state.db.pool().clone(),
        poll_interval: state.config.blockchain.poll_interval,
        batch_size: state.config.blockchain.batch_size,
        current_block: start_block,
        request_delay: Duration::from_millis(state.config.blockchain.request_delay_ms),
        max_retries: state.config.blockchain.max_retries,
        retry_delay: state.config.blockchain.retry_delay,
        direct: state.direct_handlers.clone(),
    };

    let mut shutdown_rx = state.shutdown.subscribe();
    indexer.run(&mut shutdown_rx).await
}
