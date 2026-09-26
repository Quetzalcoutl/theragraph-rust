// ─── ChainClient — Provider Seam for the Bundler ─────────────────────────────
//
// A narrow trait over the RPC + contract calls that BundlerService needs.
// The production adapter wraps HttpProvider; tests inject MockChainClient.
//
// Two adapters (HttpChainClient + MockChainClient in service/tests.rs) make
// this a real seam, not a hypothetical one.
//
// Why not generic over alloy's Provider<T, N>?  alloy::Provider is a two-
// param trait and not object-safe; making BundlerService + NonceManager
// generic over it would cascade into every caller (mempool, routes, state).
// A narrow domain trait is smaller, stable, and lets us write mocks without
// pulling in alloy's concrete transport machinery.

use std::sync::Arc;

use alloy::{
    eips::BlockNumberOrTag,
    primitives::{Address, Bytes, B256, U256},
    providers::Provider,
};
use async_trait::async_trait;
use eyre::{eyre, Result, WrapErr};
use tracing::warn;

use super::{
    contracts::{IEntryPoint, IFactory, IPaymaster},
    service::HttpProvider,
};

// ─── Trait ────────────────────────────────────────────────────────────────────

/// All RPC + contract operations BundlerService needs.  See above for why this
/// is a domain trait rather than a generic Provider bound.
#[async_trait]
pub trait ChainClient: Send + Sync {
    // ── Raw RPC ───────────────────────────────────────────────────────────────

    /// Current L1 block height.
    async fn block_number(&self) -> Result<u64>;

    /// base_fee_per_gas from the latest block head.
    async fn base_fee(&self) -> Result<u128>;

    /// Runtime bytecode at `addr` (empty = not deployed).
    async fn code_at(&self, addr: Address) -> Result<Bytes>;

    /// Broadcast a signed, RLP-encoded transaction; returns the tx hash.
    async fn send_raw_tx(&self, encoded: &[u8]) -> Result<B256>;

    /// Pending transaction count for `addr` (L1 nonce).
    async fn tx_count(&self, addr: Address) -> Result<u64>;

    // ── Contract reads ────────────────────────────────────────────────────────

    /// ERC-4337 UserOp nonce (sequential lane 0) for `sender` via EntryPoint.
    async fn userop_nonce(&self, entry_point: Address, sender: Address) -> Result<u64>;

    /// Counterfactual smart-account address for `owner` from the factory.
    async fn factory_account(&self, factory: Address, owner: Address) -> Result<Address>;

    /// Whether the paymaster's sponsorship is currently active.
    async fn sponsorship_active(&self, paymaster: Address) -> Result<bool>;

    /// eth_call: handleOps on the EntryPoint (simulation — no broadcast).
    /// Returns Ok(()) if all ops are valid; Err with the revert message otherwise.
    async fn simulate_handle_ops(
        &self,
        entry_point: Address,
        ops:         Vec<IEntryPoint::PackedUserOperation>,
        beneficiary: Address,
    ) -> Result<()>;

    // ── Health / verify ───────────────────────────────────────────────────────

    /// Paymaster deposit balance on the EntryPoint.
    async fn entry_point_balance(&self, entry_point: Address, paymaster: Address)
        -> Result<U256>;

    /// `verifyingSigner` on the paymaster contract.
    async fn verifying_signer(&self, paymaster: Address) -> Result<Address>;

    /// `getDeposit` on the paymaster contract.
    async fn paymaster_deposit(&self, paymaster: Address) -> Result<U256>;

    /// ETH balance of `addr`.
    async fn eth_balance(&self, addr: Address) -> Result<U256>;
}

// ─── Production adapter ───────────────────────────────────────────────────────

pub struct HttpChainClient {
    pub(super) provider: Arc<HttpProvider>,
}

impl HttpChainClient {
    pub fn new(provider: Arc<HttpProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ChainClient for HttpChainClient {
    async fn block_number(&self) -> Result<u64> {
        self.provider.get_block_number().await.wrap_err("get_block_number failed")
    }

    async fn base_fee(&self) -> Result<u128> {
        let block = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await
            .wrap_err("getBlock failed")?
            .ok_or_else(|| eyre!("latest block not found"))?;
        Ok(block.header.base_fee_per_gas.unwrap_or(1_000_000_000).into())
    }

    async fn code_at(&self, addr: Address) -> Result<Bytes> {
        self.provider.get_code_at(addr).await.wrap_err("getCode failed")
    }

    async fn send_raw_tx(&self, encoded: &[u8]) -> Result<B256> {
        let pending = self
            .provider
            .send_raw_transaction(encoded)
            .await
            .wrap_err("send_raw_transaction failed")?;
        Ok(*pending.tx_hash())
    }

    async fn tx_count(&self, addr: Address) -> Result<u64> {
        self.provider
            .get_transaction_count(addr)
            .await
            .wrap_err("get_transaction_count failed")
    }

    async fn userop_nonce(&self, entry_point: Address, sender: Address) -> Result<u64> {
        use alloy::primitives::Uint;
        type U192 = Uint<192, 3>;

        let ep     = IEntryPoint::new(entry_point, self.provider.clone());
        let nonce: U256 = ep
            .getNonce(sender, U192::ZERO)
            .call()
            .await
            .wrap_err("getNonce RPC failed")?;
        let seq: u64 = (nonce & U256::from(u64::MAX))
            .try_into()
            .wrap_err("nonce extraction failed")?;
        Ok(seq)
    }

    async fn factory_account(&self, factory: Address, owner: Address) -> Result<Address> {
        let f = IFactory::new(factory, self.provider.clone());
        let addr: Address = f.getAddress(owner, U256::ZERO).call().await?;
        Ok(addr)
    }

    async fn sponsorship_active(&self, paymaster: Address) -> Result<bool> {
        let pm     = IPaymaster::new(paymaster, self.provider.clone());
        let active: bool = pm.sponsorshipActive().call().await?;
        Ok(active)
    }

    async fn simulate_handle_ops(
        &self,
        entry_point: Address,
        ops:         Vec<IEntryPoint::PackedUserOperation>,
        beneficiary: Address,
    ) -> Result<()> {
        let ep = IEntryPoint::new(entry_point, self.provider.clone());
        ep.handleOps(ops, beneficiary)
            .call()
            .await
            .map_err(|e| eyre!("UserOp simulation failed: {e}"))?;
        Ok(())
    }

    async fn entry_point_balance(&self, entry_point: Address, paymaster: Address)
        -> Result<U256>
    {
        let ep: U256 = IEntryPoint::new(entry_point, self.provider.clone())
            .balanceOf(paymaster).call().await.wrap_err("balanceOf failed")?;
        Ok(ep)
    }

    async fn verifying_signer(&self, paymaster: Address) -> Result<Address> {
        let pm = IPaymaster::new(paymaster, self.provider.clone());
        match pm.verifyingSigner().call().await {
            Ok(addr) => Ok(addr),
            Err(e)   => { warn!("Could not fetch verifyingSigner: {e}"); Err(eyre!(e)) }
        }
    }

    async fn paymaster_deposit(&self, paymaster: Address) -> Result<U256> {
        let pm = IPaymaster::new(paymaster, self.provider.clone());
        match pm.getDeposit().call().await {
            Ok(d)  => Ok(d),
            Err(e) => { warn!("Could not fetch deposit: {e}"); Err(eyre!(e)) }
        }
    }

    async fn eth_balance(&self, addr: Address) -> Result<U256> {
        self.provider.get_balance(addr).await.wrap_err("get_balance failed")
    }
}
