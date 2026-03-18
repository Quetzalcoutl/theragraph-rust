// ─── Bundler Core Service ─────────────────────────────────────────────────────
//
// Architecture note (Diego B. + Boris Staal / Yalantis + Parity Technologies):
//   Plain RootProvider<Http<Client>> for reads + manual EIP-1559 signing for
//   writes.  Gives the bundler explicit control over every fee/nonce parameter.

use std::{sync::Arc, time::Duration};

use alloy::{
    consensus::{SignableTransaction, TxEip1559, TxEnvelope},
    eips::{eip2718::Encodable2718, BlockNumberOrTag},
    primitives::{Address, Bytes, TxKind, U256, B256, Uint},
    providers::{Provider, ProviderBuilder, RootProvider},
    signers::{local::PrivateKeySigner, SignerSync},
    sol_types::SolCall,
    transports::http::{Client, Http},
};
use eyre::{bail, eyre, Result, WrapErr};
use tokio::time::sleep;
use tracing::{error, info, warn};

use super::{
    account_nonce::UserOpNonceManager,
    config::Config,
    contracts::{IAccount, IEntryPoint, IFactory, IPaymaster},
    gas::{
        deployment_verification_gas, pack_account_gas_limits, pack_gas_fees, scale_call_gas,
    },
    hash::compute_user_op_hash,
    nonce::NonceManager,
    paymaster::PaymasterSigner,
    types::{Call, PackedUserOperation},
};

type U192 = Uint<192, 3>;
pub type HttpProvider = RootProvider<Http<Client>>;

const MAX_RETRIES: u32 = 2;
const BASE_RETRY_MS: u64 = 1_500;
const HANDLE_OPS_GAS_LIMIT: u128 = 3_000_000;

// ─── Batch simulation outcome ────────────────────────────────────────────────

/// Returned by `BundlerService::simulate_batch`.
#[derive(Debug)]
pub enum BatchSimOutcome {
    /// All ops passed simulation — safe to broadcast.
    Ok,
    /// Op at `index` failed with a known AA error.  The caller should remove
    /// it from the batch, send the error to its result channel, and retry
    /// simulation on the remaining ops.
    BadOp { index: usize, reason: String },
    /// RPC / infrastructure failure — abort the whole batch and retry later.
    RpcError(String),
}

// ─── Parse FailedOp from alloy error string ───────────────────────────────────
//
// alloy formats EntryPoint custom errors in two ways depending on whether the
// ABI was decoded:
//   decoded:   "FailedOp { opIndex: 1, reason: \"AA25 invalid account nonce\" }"
//   raw:       "FailedOp(1, \"AA25 ...\")"  (older alloy)
//   revert:    "execution reverted: FailedOp(opIndex: 1, reason: \"AA25...\")"
fn parse_failed_op(msg: &str) -> Option<(usize, String)> {
    if !msg.contains("FailedOp") {
        return None;
    }

    // Extract opIndex ── try "opIndex: N" then positional "FailedOp(N,"
    let idx: usize = if let Some(p) = msg.find("opIndex: ") {
        let s = &msg[p + 9..];
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        s[..end].trim().parse().ok()?
    } else if let Some(p) = msg.find("FailedOp(") {
        let s = &msg[p + 9..];
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        s[..end].trim().parse().ok()?
    } else {
        return None;
    };

    // Extract reason ── try `reason: "…"` then first `"AA…` fragment
    let reason = if let Some(p) = msg.find("reason: \"") {
        let s = &msg[p + 9..];
        s[..s.find('"').unwrap_or(s.len())].to_string()
    } else if let Some(p) = msg.find("\"AA") {
        let s = &msg[p + 1..];
        s[..s.find('"').unwrap_or(s.len())].to_string()
    } else {
        "unknown AA error".to_string()
    };

    Some((idx, reason))
}

// ─── Service ──────────────────────────────────────────────────────────────────

/// The main bundler instance.  Cheap to clone — internals are behind `Arc`.
#[derive(Clone)]
pub struct BundlerService {
    config:        Arc<Config>,
    provider:      Arc<HttpProvider>,
    signer:        PrivateKeySigner,
    paymaster:     Arc<PaymasterSigner>,
    /// Serialises L1 nonce allocation across all concurrent submissions.
    nonce_manager: NonceManager,
}

impl BundlerService {
    pub fn new(config: Arc<Config>) -> Result<Self> {
        let signer: PrivateKeySigner = config
            .private_key
            .parse()
            .wrap_err("Invalid PRIVATE_KEY")?;

        let provider: HttpProvider = ProviderBuilder::new()
            .on_http(config.rpc_url.parse().wrap_err("Invalid RPC_URL")?);

        let paymaster = Arc::new(PaymasterSigner::new(&config)?);
        let provider  = Arc::new(provider);
        let signer_addr = signer.address();
        let nonce_manager = NonceManager::new(provider.clone(), signer_addr);

        Ok(Self {
            config,
            provider,
            signer,
            paymaster,
            nonce_manager,
        })
    }

    pub fn signer_address(&self) -> Address {
        self.signer.address()
    }

    pub fn provider(&self) -> &HttpProvider {
        &self.provider
    }

    /// Block number + paymaster deposit fetched in one round-trip.
    pub async fn health_stats(&self) -> Result<(u64, U256, bool)> {
        let ep = IEntryPoint::new(self.config.entry_point, self.provider.clone());
        let pm = IPaymaster::new(self.config.paymaster,  self.provider.clone());

        let bal_future    = ep.balanceOf(self.config.paymaster);
        let active_future = pm.sponsorshipActive();

        let (block_res, deposit_res, active_res) = tokio::join!(
            self.provider.get_block_number(),
            bal_future.call(),
            active_future.call(),
        );

        Ok((
            block_res.wrap_err("getBlockNumber failed")?,
            deposit_res.wrap_err("balanceOf failed")?._0,
            active_res.wrap_err("sponsorshipActive failed")?._0,
        ))
    }

    pub async fn get_smart_account_address(&self, owner: Address) -> Result<Address> {
        let factory = IFactory::new(self.config.factory, self.provider.clone());
        let result  = factory.getAddress(owner, U256::ZERO).call().await?;
        Ok(result.predicted)
    }

    pub async fn get_nonce(&self, sender: Address) -> Result<U256> {
        let ep     = IEntryPoint::new(self.config.entry_point, self.provider.clone());
        let result = ep.getNonce(sender, U192::ZERO).call().await?;
        Ok(result.nonce)
    }

    pub async fn is_deployed(&self, address: Address) -> Result<bool> {
        let code = self
            .provider
            .get_code_at(address)
            .await
            .wrap_err("getCode failed")?;
        Ok(!code.is_empty())
    }

    pub async fn get_gas_fees(&self) -> Result<(u128, u128)> {
        let block = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Latest, false.into())
            .await
            .wrap_err("getBlock failed")?
            .ok_or_else(|| eyre!("Latest block not found"))?;

        let base_fee: u128 = block
            .header
            .base_fee_per_gas
            .unwrap_or(1_000_000_000)
            .into();

        let priority_fee: u128 = if self.config.chain_id == 100 {
            1_000_000_000
        } else {
            1_500_000_000
        };

        let max_fee = base_fee as u128 * 2 + priority_fee;
        Ok((priority_fee, max_fee))
    }

    fn build_call_data(&self, calls: &[Call]) -> Bytes {
        if calls.len() == 1 {
            let c = &calls[0];
            let encoded = IAccount::executeCall {
                target: c.target,
                value:  c.value.map(|v| v.0).unwrap_or(U256::ZERO),
                data:   c.data.clone(),
            }
            .abi_encode();
            Bytes::from(encoded)
        } else {
            let encoded = IAccount::executeBatchCall {
                targets: calls.iter().map(|c| c.target).collect(),
                values:  calls.iter().map(|c| c.value.map(|v| v.0).unwrap_or(U256::ZERO)).collect(),
                datas:   calls.iter().map(|c| c.data.clone()).collect(),
            }
            .abi_encode();
            Bytes::from(encoded)
        }
    }

    fn build_init_code(&self, owner: Address) -> Bytes {
        let factory_call = IFactory::createAccountCall {
            owner,
            salt: U256::ZERO,
        }
        .abi_encode();

        let mut init = Vec::with_capacity(20 + factory_call.len());
        init.extend_from_slice(self.config.factory.as_slice());
        init.extend_from_slice(&factory_call);
        Bytes::from(init)
    }

    /// Build a paymaster-sponsored UserOp ready for the user to sign.
    ///
    /// `op_nonce_mgr` is the per-sender ERC-4337 nonce manager.  Pass it from
    /// `BundlerState` so that concurrent /sponsor requests for the same sender
    /// receive strictly sequential nonces instead of all getting the same
    /// on-chain value.
    pub async fn build_sponsored_user_op(
        &self,
        sender:       Address,
        calls:        &[Call],
        owner_address: Option<Address>,
        op_nonce_mgr: &UserOpNonceManager,
    ) -> Result<(PackedUserOperation, B256)> {
        if calls.is_empty() {
            bail!("calls array must not be empty");
        }

        let deployed = self.is_deployed(sender).await?;
        let init_code = if deployed {
            Bytes::new()
        } else {
            let owner = owner_address
                .ok_or_else(|| eyre!("Account not deployed and ownerAddress not provided"))?;
            info!("First UserOp — initCode will deploy account for {owner}");
            self.build_init_code(owner)
        };

        // Reserve the next sequential UserOp nonce for this sender.
        // op_nonce_mgr serialises concurrent /sponsor calls so each gets a
        // distinct, monotonically-increasing nonce even before past ops confirm.
        let (nonce_res, fees_res) = tokio::join!(
            op_nonce_mgr.reserve(sender, &self.provider),
            self.get_gas_fees(),
        );

        let nonce = nonce_res.wrap_err("UserOp nonce reservation failed")?;
        let (priority_fee, max_fee) = fees_res.wrap_err("fee estimation failed")?;

        let call_data = self.build_call_data(calls);

        let verification_gas = if deployed {
            self.config.gas.verification_gas_limit as u128
        } else {
            deployment_verification_gas(self.config.gas.verification_gas_limit) as u128
        };

        let call_gas = scale_call_gas(self.config.gas.call_gas_limit, calls.len()) as u128;

        let account_gas_limits =
            pack_account_gas_limits(verification_gas, call_gas);
        let gas_fees = pack_gas_fees(priority_fee, max_fee);

        let mut user_op = PackedUserOperation {
            sender,
            nonce,
            init_code,
            call_data,
            account_gas_limits,
            pre_verification_gas: U256::from(self.config.gas.pre_verification_gas),
            gas_fees,
            paymaster_and_data: Bytes::new(),
            signature: Bytes::new(),
        };

        let expiry = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32
            + 600;

        user_op.paymaster_and_data =
            self.paymaster.sign_paymaster_data(&user_op, 0, expiry)?;

        let user_op_hash = compute_user_op_hash(
            &user_op,
            &self.config.entry_point,
            self.config.chain_id,
        );

        Ok((user_op, user_op_hash))
    }

    /// Simulate then submit a single signed UserOp.  Still used for the
    /// legacy direct-submit path; high-throughput callers should prefer the
    /// mempool → `submit_batch` path.
    pub async fn submit_user_op(&self, user_op: PackedUserOperation) -> Result<B256> {
        self.simulate(&user_op).await?;
        self.submit_batch(&[user_op]).await
    }

    /// Submit a slice of UserOps as one `handleOps` transaction with retry.
    ///
    /// This is the hot path called by the batch processor.  A single L1 nonce
    /// is consumed for the whole batch, and the nonce manager ensures no two
    /// concurrent batches ever race.
    pub async fn submit_batch(&self, user_ops: &[PackedUserOperation]) -> Result<B256> {
        if user_ops.is_empty() {
            return Err(eyre!("submit_batch: empty ops slice"));
        }

        let mut last_err = eyre!("no attempts made");

        for attempt in 0..=MAX_RETRIES {
            match self.do_submit_batch(user_ops).await {
                Ok(tx_hash) => return Ok(tx_hash),
                Err(e) => {
                    let msg = e.to_string().to_lowercase();
                    // Hard failures — no point retrying
                    if msg.contains("revert")
                        || msg.contains("simulation")
                        || msg.contains("signature")
                        || msg.contains("\"aa")
                        || msg.starts_with("aa")
                    {
                        return Err(e);
                    }

                    if attempt < MAX_RETRIES {
                        let delay = Duration::from_millis(BASE_RETRY_MS << attempt);
                        warn!(
                            "Submit attempt {} failed ({}), retrying in {}ms…",
                            attempt + 1, e, delay.as_millis()
                        );
                        // Re-sync nonce from chain before the next attempt
                        if let Err(e2) = self.nonce_manager.resync().await {
                            warn!("Nonce resync failed: {e2}");
                        }
                        sleep(delay).await;
                    }
                    last_err = e;
                }
            }
        }

        Err(last_err)
    }

    /// eth_call simulation — validates the UserOp against the EntryPoint
    /// without broadcasting.  Only safe to call when the op's nonce matches
    /// the current on-chain nonce (first op for a sender).  For batches of
    /// pending ops use `simulate_batch` instead.
    pub async fn simulate(&self, user_op: &PackedUserOperation) -> Result<()> {
        let ep      = IEntryPoint::new(self.config.entry_point, self.provider.clone());
        let sol_op  = self.to_sol_op(user_op);
        ep.handleOps(vec![sol_op], self.signer_address())
            .call()
            .await
            .map_err(|e| eyre!("UserOp simulation failed: {}", e))?;
        Ok(())
    }

    /// Simulate a whole batch atomically via eth_call.
    ///
    /// Returns:
    ///   `BatchSimOutcome::Ok`              — all ops in the batch are valid.
    ///   `BatchSimOutcome::BadOp{idx,…}`   — op at index `idx` is invalid;
    ///                                        remove it and retry the rest.
    ///   `BatchSimOutcome::RpcError(msg)`  — infrastructure failure.
    ///
    /// This is the correct approach for pending-nonce batches: the EntryPoint
    /// processes ops sequentially in the eth_call, so op[0] incrementing the
    /// sender nonce makes op[1] with nonce+1 valid in the same call — unlike
    /// simulating each op individually against the current chain state.
    pub async fn simulate_batch(&self, user_ops: &[PackedUserOperation]) -> BatchSimOutcome {
        if user_ops.is_empty() {
            return BatchSimOutcome::Ok;
        }
        let sol_ops: Vec<_> = user_ops.iter().map(|op| self.to_sol_op(op)).collect();
        let ep = IEntryPoint::new(self.config.entry_point, self.provider.clone());
        match ep.handleOps(sol_ops, self.signer_address()).call().await {
            Ok(_)  => BatchSimOutcome::Ok,
            Err(e) => {
                let msg = e.to_string();
                match parse_failed_op(&msg) {
                    Some((idx, reason)) => BatchSimOutcome::BadOp { index: idx, reason },
                    None                => BatchSimOutcome::RpcError(msg),
                }
            }
        }
    }

    /// Build, sign, and broadcast a `handleOps` L1 transaction for the given
    /// UserOps slice.  Holds the nonce manager lock for the entire sign →
    /// broadcast window to guarantee no concurrent submission reuses the nonce.
    async fn do_submit_batch(&self, user_ops: &[PackedUserOperation]) -> Result<B256> {
        let sol_ops: Vec<_> = user_ops.iter().map(|op| self.to_sol_op(op)).collect();
        let beneficiary = self.signer_address();

        let calldata = Bytes::from(
            IEntryPoint::handleOpsCall {
                ops: sol_ops,
                beneficiary,
            }
            .abi_encode(),
        );

        // Fetch gas fees outside the nonce lock (network call, no ordering
        // requirement).
        let (priority_fee, max_fee) = self.get_gas_fees().await?;

        // Scale gas limit by batch size: base 3 M + 300 k per additional op.
        let batch_extra   = (user_ops.len().saturating_sub(1)) as u128 * 300_000;
        let gas_limit     = (HANDLE_OPS_GAS_LIMIT + batch_extra) as u64;

        // ── Critical section: allocate nonce, sign, broadcast ─────────────
        // The lock is held across the entire async [sign → broadcast] so no
        // concurrent do_submit_batch can reuse the same nonce.
        let mut nonce_guard = self.nonce_manager.lock().await?;
        let nonce = nonce_guard.nonce;

        let tx = TxEip1559 {
            chain_id:                 self.config.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas:          max_fee,
            max_priority_fee_per_gas: priority_fee,
            to:                       TxKind::Call(self.config.entry_point),
            value:                    U256::ZERO,
            input:                    calldata,
            access_list:              Default::default(),
        };

        let sig = self
            .signer
            .sign_hash_sync(&tx.signature_hash())
            .wrap_err("sign_hash_sync failed")?;
        let envelope = TxEnvelope::Eip1559(tx.into_signed(sig));
        let encoded  = envelope.encoded_2718();

        match self.provider.send_raw_transaction(&encoded).await {
            Ok(pending) => {
                // Optimistically advance the local nonce counter.
                nonce_guard.commit();
                let tx_hash = *pending.tx_hash();
                info!(
                    "handleOps batch({}) broadcasted: {tx_hash:#x}",
                    user_ops.len()
                );
                Ok(tx_hash)
            }
            Err(e) => {
                // On a nonce-related error re-sync before releasing the lock
                // so the next reservation starts from the correct on-chain value.
                let msg = e.to_string().to_lowercase();
                if msg.contains("nonce")
                    || msg.contains("replacement")
                    || msg.contains("already known")
                    || msg.contains("underpriced")
                {
                    nonce_guard.resync().await;
                }
                Err(eyre!("handleOps send_raw_transaction failed"))
            }
        }
        // nonce_guard dropped here — lock released
    }

    fn to_sol_op(&self, op: &PackedUserOperation) -> IEntryPoint::PackedUserOperation {
        IEntryPoint::PackedUserOperation {
            sender:              op.sender,
            nonce:               op.nonce,
            initCode:            op.init_code.clone(),
            callData:            op.call_data.clone(),
            accountGasLimits:    op.account_gas_limits,
            preVerificationGas:  op.pre_verification_gas,
            gasFees:             op.gas_fees,
            paymasterAndData:    op.paymaster_and_data.clone(),
            signature:           op.signature.clone(),
        }
    }

    pub async fn verify_paymaster_config(&self) {
        let pm = IPaymaster::new(self.config.paymaster, self.provider.clone());

        match pm.verifyingSigner().call().await {
            Ok(r) => {
                let on_chain = r._0;
                let local    = self.signer_address();
                let ok = on_chain.to_checksum(None).eq_ignore_ascii_case(
                    &local.to_checksum(None),
                );
                if ok {
                    info!("Paymaster verifyingSigner: {on_chain} ✅ matches bundler key");
                } else {
                    error!(
                        "SIGNER MISMATCH — on-chain={on_chain}, local={local}. \
                         UserOps will fail AA34. Update PRIVATE_KEY or call paymaster.setSigner()."
                    );
                }
            }
            Err(e) => warn!("Could not fetch verifyingSigner: {e}"),
        }

        match pm.getDeposit().call().await {
            Ok(r) => info!("Paymaster deposit: {} wei", r._0),
            Err(e) => warn!("Could not fetch deposit: {e}"),
        }

        match pm.sponsorshipActive().call().await {
            Ok(r) => info!("Sponsorship active: {}", r._0),
            Err(e) => warn!("Could not fetch sponsorshipActive: {e}"),
        }
    }

    /// Send plain ETH from the bundler signer to `to`.
    pub async fn send_eth(&self, to: Address, value: U256) -> Result<B256> {
        let nonce = self
            .provider
            .get_transaction_count(self.signer_address())
            .await
            .wrap_err("get_transaction_count failed")?;

        let (priority_fee, max_fee) = self.get_gas_fees().await?;

        let tx = TxEip1559 {
            chain_id:                 self.config.chain_id,
            nonce,
            gas_limit:                21_000,
            max_fee_per_gas:          max_fee,
            max_priority_fee_per_gas: priority_fee,
            to:                       TxKind::Call(to),
            value,
            input:                    Bytes::new(),
            access_list:              Default::default(),
        };

        let sig = self.signer
            .sign_hash_sync(&tx.signature_hash())
            .wrap_err("sign failed")?;
        let envelope = TxEnvelope::Eip1559(tx.into_signed(sig));

        let encoded = envelope.encoded_2718();

        let pending = self.provider
            .send_raw_transaction(&encoded)
            .await
            .wrap_err("send_raw_transaction failed")?;

        Ok(*pending.tx_hash())
    }
}
