// ─── Bundler Core Service ─────────────────────────────────────────────────────
//
// Architecture note (Diego B. + Boris Staal / Yalantis + Parity Technologies):
//   Plain RootProvider<Http<Client>> for reads + manual EIP-1559 signing for
//   writes.  Gives the bundler explicit control over every fee/nonce parameter.
//
// Provider seam (ChainClient trait):
//   BundlerService holds Arc<dyn ChainClient> rather than the concrete
//   RootProvider.  HttpChainClient is the production adapter; tests inject
//   MockChainClient.  See chain_client.rs for the design rationale.

use std::{sync::Arc, time::Duration};

use alloy::{
    consensus::{SignableTransaction, TxEip1559, TxEnvelope},
    eips::eip2718::Encodable2718,
    network::Ethereum,
    primitives::{Address, Bytes, TxKind, U256, B256, Uint},
    rpc::client::ClientBuilder,
    signers::{local::PrivateKeySigner, SignerSync},
    transports::http::Client,
    providers::RootProvider,
};
use eyre::{bail, eyre, Result, WrapErr};
use tokio::time::sleep;
use tracing::{error, info, warn};

use super::{
    account_nonce::UserOpNonceManager,
    chain_client::{ChainClient, HttpChainClient},
    config::Config,
    contracts::IEntryPoint,
    error::{BatchSimOutcome, BundlerError, parse_failed_op},
    gas::{
        deployment_verification_gas, pack_account_gas_limits, pack_gas_fees, scale_call_gas,
    },
    hash::compute_user_op_hash,
    nonce::NonceManager,
    op_encoding::{encode_call_data, encode_init_code, to_entry_point_op},
    paymaster::PaymasterSigner,
    types::{Call, PackedUserOperation},
};

#[allow(dead_code)]
type U192 = Uint<192, 3>;
pub type HttpProvider = RootProvider<Ethereum>;

const MAX_RETRIES: u32 = 2;
const BASE_RETRY_MS: u64 = 1_500;
const HANDLE_OPS_GAS_LIMIT: u128 = 3_000_000;

// ─── Service ──────────────────────────────────────────────────────────────────

/// The main bundler instance.  Cheap to clone — internals are behind `Arc`.
#[derive(Clone)]
pub struct BundlerService {
    config:        Arc<Config>,
    client:        Arc<dyn ChainClient>,
    signer:        PrivateKeySigner,
    paymaster:     Arc<PaymasterSigner>,
    /// Serialises L1 nonce allocation across all concurrent submissions.
    nonce_manager: NonceManager,
}

impl BundlerService {
    /// Production constructor — builds HttpChainClient from config.
    pub fn new(config: Arc<Config>) -> Result<Self> {
        let signer: PrivateKeySigner = config
            .private_key
            .parse()
            .wrap_err("Invalid PRIVATE_KEY")?;

        let rpc_url: reqwest::Url = config.rpc_url.parse().wrap_err("Invalid RPC_URL")?;
        let rpc_http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .wrap_err("Failed to build RPC HTTP client")?;
        let rpc_client = ClientBuilder::default().http_with_client(rpc_http, rpc_url);
        let provider: HttpProvider = RootProvider::new(rpc_client);
        let provider = Arc::new(provider);
        let client: Arc<dyn ChainClient> = Arc::new(HttpChainClient::new(provider));

        Self::with_client(config, signer, client)
    }

    /// Injection constructor — used by tests to supply a mock client.
    pub fn with_client(
        config: Arc<Config>,
        signer: PrivateKeySigner,
        client: Arc<dyn ChainClient>,
    ) -> Result<Self> {
        let paymaster     = Arc::new(PaymasterSigner::with_signer(signer.clone(), &config));
        let signer_addr   = signer.address();
        let nonce_manager = NonceManager::new(client.clone(), signer_addr);

        Ok(Self { config, client, signer, paymaster, nonce_manager })
    }

    pub fn signer_address(&self) -> Address {
        self.signer.address()
    }

    /// Block number + paymaster deposit fetched in one round-trip.
    pub async fn health_stats(&self) -> Result<(u64, U256, bool)> {
        let (block_res, deposit_res, active_res) = tokio::join!(
            self.client.block_number(),
            self.client.entry_point_balance(self.config.entry_point, self.config.paymaster),
            self.client.sponsorship_active(self.config.paymaster),
        );

        Ok((
            block_res.wrap_err("getBlockNumber failed")?,
            deposit_res.wrap_err("balanceOf failed")?,
            active_res.wrap_err("sponsorshipActive failed")?,
        ))
    }

    pub async fn get_smart_account_address(&self, owner: Address) -> Result<Address> {
        self.client.factory_account(self.config.factory, owner).await
    }

    #[allow(dead_code)]
    pub async fn get_nonce(&self, sender: Address) -> Result<U256> {
        let seq = self.client.userop_nonce(self.config.entry_point, sender).await?;
        Ok(U256::from(seq))
    }

    pub async fn eth_balance(&self, addr: Address) -> Result<U256> {
        self.client.eth_balance(addr).await
    }

    pub async fn is_deployed(&self, address: Address) -> Result<bool> {
        let code = self.client.code_at(address).await?;
        Ok(!code.is_empty())
    }

    pub async fn get_gas_fees(&self) -> Result<(u128, u128)> {
        let base_fee = self.client.base_fee().await?;

        // Gnosis Chain (chain_id 100) uses a lower priority fee than mainnet.
        // Policy lives here in config, not inside the RPC fetch.
        let priority_fee: u128 = self.config.priority_fee_gwei;

        let max_fee = base_fee
            .checked_mul(2)
            .ok_or_else(|| eyre!("base_fee overflow: base_fee={base_fee}"))?
            .checked_add(priority_fee)
            .ok_or_else(|| eyre!("max_fee overflow: base_fee={base_fee} priority_fee={priority_fee}"))?;
        Ok((priority_fee, max_fee))
    }

    /// Build a paymaster-sponsored UserOp ready for the user to sign.
    ///
    /// `op_nonce_mgr` is the per-sender ERC-4337 nonce manager.  Pass it from
    /// `BundlerState` so that concurrent /sponsor requests for the same sender
    /// receive strictly sequential nonces instead of all getting the same
    /// on-chain value.
    pub async fn build_sponsored_user_op(
        &self,
        sender:        Address,
        calls:         &[Call],
        owner_address: Option<Address>,
        op_nonce_mgr:  &UserOpNonceManager,
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
            encode_init_code(self.config.factory, owner)
        };

        let (nonce_res, fees_res, active_res) = tokio::join!(
            op_nonce_mgr.reserve(sender, self.client.as_ref()),
            self.get_gas_fees(),
            self.client.sponsorship_active(self.config.paymaster),
        );

        let nonce = nonce_res.wrap_err("UserOp nonce reservation failed")?;
        let (priority_fee, max_fee) = fees_res.wrap_err("fee estimation failed")?;
        match active_res {
            Ok(active) if !active => bail!("Paymaster sponsorship is not active"),
            Err(e)                => warn!("Could not verify sponsorship status (proceeding): {e}"),
            Ok(_)                 => {}
        }

        let call_data = encode_call_data(calls);

        let verification_gas = if deployed {
            self.config.gas.verification_gas_limit as u128
        } else {
            deployment_verification_gas(self.config.gas.verification_gas_limit) as u128
        };

        let call_gas          = scale_call_gas(self.config.gas.call_gas_limit, calls.len()) as u128;
        let account_gas_limits = pack_account_gas_limits(verification_gas, call_gas);
        let gas_fees           = pack_gas_fees(priority_fee, max_fee);

        let mut user_op = PackedUserOperation {
            sender,
            nonce,
            init_code,
            call_data,
            account_gas_limits,
            pre_verification_gas: U256::from(self.config.gas.pre_verification_gas),
            gas_fees,
            paymaster_and_data: Bytes::new(),
            signature:          Bytes::new(),
        };

        let expiry = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(600);

        user_op.paymaster_and_data =
            self.paymaster.sign_paymaster_data(&user_op, expiry, 0)?;

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
        let sol_op = to_entry_point_op(user_op);
        self.client
            .simulate_handle_ops(self.config.entry_point, vec![sol_op], self.signer_address())
            .await
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
        let sol_ops: Vec<_> = user_ops.iter().map(|op| to_entry_point_op(op)).collect();
        match self
            .client
            .simulate_handle_ops(self.config.entry_point, sol_ops, self.signer_address())
            .await
        {
            Ok(_)  => BatchSimOutcome::Ok,
            Err(e) => {
                let msg = e.to_string();
                match parse_failed_op(&msg) {
                    Ok((idx, reason)) => BatchSimOutcome::BadOp { index: idx, reason },
                    Err(BundlerError::FailedOp { op_index, reason }) => {
                        BatchSimOutcome::BadOp { index: op_index, reason }
                    }
                    Err(BundlerError::Other(_)) => BatchSimOutcome::RpcError(msg),
                }
            }
        }
    }

    /// Build, sign, and broadcast a `handleOps` L1 transaction.
    async fn do_submit_batch(&self, user_ops: &[PackedUserOperation]) -> Result<B256> {
        use alloy::sol_types::SolCall;

        let sol_ops: Vec<_> = user_ops.iter().map(|op| to_entry_point_op(op)).collect();
        let calldata = Bytes::from(
            IEntryPoint::handleOpsCall {
                ops:         sol_ops,
                beneficiary: self.signer_address(),
            }
            .abi_encode(),
        );

        let (priority_fee, max_fee) = self.get_gas_fees().await?;

        let batch_extra = (user_ops.len().saturating_sub(1)) as u128 * 300_000;
        let total_gas   = HANDLE_OPS_GAS_LIMIT + batch_extra;
        let gas_limit   = u64::try_from(total_gas)
            .map_err(|_| eyre!("gas limit overflow: {} ops = {} gas", user_ops.len(), total_gas))?;

        self.sign_and_broadcast(
            TxKind::Call(self.config.entry_point),
            U256::ZERO,
            calldata,
            gas_limit,
            priority_fee,
            max_fee,
        )
        .await
        .inspect(|tx_hash| {
            info!("handleOps batch({}) broadcasted: {tx_hash:#x}", user_ops.len())
        })
    }

    /// Send plain ETH from the bundler signer to `to`.
    ///
    /// Acquires the nonce_manager mutex to prevent a nonce collision with a
    /// concurrent do_submit_batch call — both paths share the same EOA signer.
    pub async fn send_eth(&self, to: Address, value: U256) -> Result<B256> {
        let (priority_fee, max_fee) = self.get_gas_fees().await?;
        self.sign_and_broadcast(TxKind::Call(to), value, Bytes::new(), 21_000, priority_fee, max_fee)
            .await
    }

    /// Sign and broadcast one EIP-1559 transaction.  Holds the nonce lock for
    /// the entire sign → broadcast window so no two concurrent calls share a nonce.
    ///
    /// Nonce-related errors trigger a resync before the lock is released so the
    /// next reservation starts from the correct on-chain value.
    async fn sign_and_broadcast(
        &self,
        to:           TxKind,
        value:        U256,
        input:        Bytes,
        gas_limit:    u64,
        priority_fee: u128,
        max_fee:      u128,
    ) -> Result<B256> {
        let mut nonce_guard = self.nonce_manager.lock().await?;
        let nonce = nonce_guard.nonce;

        let tx = TxEip1559 {
            chain_id:                 self.config.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas:          max_fee,
            max_priority_fee_per_gas: priority_fee,
            to,
            value,
            input,
            access_list:              Default::default(),
        };

        let sig      = self.signer.sign_hash_sync(&tx.signature_hash()).wrap_err("sign failed")?;
        let envelope = TxEnvelope::Eip1559(tx.into_signed(sig));
        let encoded  = envelope.encoded_2718();

        match self.client.send_raw_tx(&encoded).await {
            Ok(tx_hash) => {
                nonce_guard.commit();
                Ok(tx_hash)
            }
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("nonce")
                    || msg.contains("replacement")
                    || msg.contains("already known")
                    || msg.contains("underpriced")
                {
                    nonce_guard.resync().await;
                }
                Err(e).wrap_err("send_raw_transaction failed")
            }
        }
        // nonce_guard dropped here — lock released
    }

    pub async fn verify_paymaster_config(&self) {
        match self.client.verifying_signer(self.config.paymaster).await {
            Ok(on_chain) => {
                let local = self.signer_address();
                let ok    = on_chain.to_checksum(None).eq_ignore_ascii_case(&local.to_checksum(None));
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

        match self.client.paymaster_deposit(self.config.paymaster).await {
            Ok(d)  => info!("Paymaster deposit: {} wei", d),
            Err(e) => warn!("Could not fetch deposit: {e}"),
        }

        match self.client.sponsorship_active(self.config.paymaster).await {
            Ok(a)  => info!("Sponsorship active: {a}"),
            Err(e) => warn!("Could not fetch sponsorshipActive: {e}"),
        }
    }
}

impl super::mempool::BatchSubmitter for BundlerService {
    fn simulate_batch<'a>(
        &'a self,
        ops: &'a [PackedUserOperation],
    ) -> impl std::future::Future<Output = BatchSimOutcome> + Send + 'a {
        self.simulate_batch(ops)
    }

    fn submit_batch<'a>(
        &'a self,
        ops: &'a [PackedUserOperation],
    ) -> impl std::future::Future<Output = eyre::Result<B256>> + Send + 'a {
        self.submit_batch(ops)
    }
}
