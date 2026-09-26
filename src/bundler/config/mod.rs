// ─── Bundler Configuration ────────────────────────────────────────────────────
//
// All critical values come from environment variables.  dotenvy handles .env
// loading so the binary itself stays 12-factor clean.
//
// Architecture note (Boris Staal / Parity Technologies):
//   Field validation is done eagerly at start-up so that a misconfigured
//   deployment fails fast before accepting any network traffic.

use alloy::primitives::Address;
use eyre::{eyre, Result, WrapErr};
use std::str::FromStr;

/// Central bundler configuration — cloned cheaply via Arc<Config>.
#[derive(Debug, Clone)]
pub struct Config {
    // ── Signer ────────────────────────────────────────────────────────────────
    /// Raw 32-byte private key (hex, with or without 0x prefix).
    pub private_key: String,

    // ── RPC ───────────────────────────────────────────────────────────────────
    pub rpc_url: String,
    pub chain_id: u64,

    // ── Smart contracts ───────────────────────────────────────────────────────
    pub entry_point: Address,
    pub paymaster: Address,
    pub factory: Address,

    /// Optional V2 TheraAccount implementation (adds onERC721Received).
    pub account_impl_v2: Option<Address>,

    // ── Redis (optional — falls back to in-memory DashMap) ────────────────────
    pub redis_url: Option<String>,

    // ── Fee policy ────────────────────────────────────────────────────────────
    /// EIP-1559 priority fee (wei).  Defaults differ by chain: Gnosis (100)
    /// uses 1 Gwei; all others use 1.5 Gwei.  Set by Config::from_env() at
    /// parse time so get_gas_fees() is a pure fetch, not a policy branch.
    pub priority_fee_gwei: u128,

    // ── Default gas limits ────────────────────────────────────────────────────
    pub gas: GasConfig,
}

#[derive(Debug, Clone)]
pub struct GasConfig {
    pub verification_gas_limit: u64,
    /// base for single call; +100_000 per extra call
    pub call_gas_limit: u64,
    pub pre_verification_gas: u64,
    pub paymaster_verification_gas_limit: u64,
    pub paymaster_post_op_gas_limit: u64,
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn required(key: &str) -> Result<String> {
    std::env::var(key).map_err(|_| eyre!("Missing required environment variable: {key}"))
}

fn optional(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn parse_address(raw: &str) -> Result<Address> {
    let s = if raw.starts_with("0x") || raw.starts_with("0X") {
        raw.to_owned()
    } else {
        format!("0x{raw}")
    };
    Address::from_str(&s).wrap_err_with(|| format!("Invalid address: {raw}"))
}

fn normalize_hex_key(raw: &str) -> String {
    if raw.starts_with("0x") || raw.starts_with("0X") {
        raw.to_owned()
    } else {
        format!("0x{raw}")
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────


// ─── Loader ───────────────────────────────────────────────────────────────────

impl Config {
    /// Load bundler configuration from the environment.
    ///
    /// Returns an error if required variables (PRIVATE_KEY, PAYMASTER_ADDRESS,
    /// FACTORY_ADDRESS) are missing — the caller treats this as "bundler not
    /// configured" and disables bundler routes gracefully.
    pub fn from_env() -> Result<Self> {
        // .env is loaded by the main config — no need to call dotenvy again

        let private_key = normalize_hex_key(&required("PRIVATE_KEY")?);

        let entry_point = parse_address(
            &optional("ENTRYPOINT_ADDRESS")
                .unwrap_or_else(|| "0x0000000071727De22E5E9d8BAf0edAc6f37da032".into()),
        )?;

        let paymaster = parse_address(&required("PAYMASTER_ADDRESS")?)?;
        let factory = parse_address(&required("FACTORY_ADDRESS")?)?;

        let account_impl_v2 = optional("ACCOUNT_IMPL_V2")
            .map(|s| parse_address(&s))
            .transpose()?;

        Ok(Self {
            private_key,

            rpc_url: required("RPC_URL")?,
            chain_id: optional("CHAIN_ID")
                .and_then(|c| c.parse().ok())
                .unwrap_or(11155111u64),

            entry_point,
            paymaster,
            factory,
            account_impl_v2,

            redis_url: optional("REDIS_URL"),

            // Gnosis (chain_id 100): 1 Gwei. All others: 1.5 Gwei.
            // Policy set once here so get_gas_fees() is a pure RPC fetch.
            priority_fee_gwei: if optional("CHAIN_ID")
                .and_then(|c| c.parse::<u64>().ok())
                .unwrap_or(11155111) == 100
            {
                1_000_000_000
            } else {
                1_500_000_000
            },

            gas: GasConfig {
                verification_gas_limit: optional("GAS_VERIFICATION_LIMIT")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(300_000),
                call_gas_limit: optional("GAS_CALL_LIMIT")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(500_000),
                pre_verification_gas: optional("GAS_PRE_VERIFICATION")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(55_000),
                paymaster_verification_gas_limit: optional("GAS_PM_VERIFICATION_LIMIT")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(150_000),
                paymaster_post_op_gas_limit: optional("GAS_PM_POST_OP_LIMIT")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(50_000),
            },
        })
    }

}

#[cfg(test)]
mod tests;
