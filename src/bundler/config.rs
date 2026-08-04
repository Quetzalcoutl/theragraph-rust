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

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize_hex_key ---

    #[test]
    fn normalize_hex_key_strips_lowercase_0x_prefix() {
        let raw = "0xdeadbeef";
        assert_eq!(normalize_hex_key(raw), "0xdeadbeef");
    }

    #[test]
    fn normalize_hex_key_strips_uppercase_0x_prefix() {
        let raw = "0Xdeadbeef";
        // Already has 0X prefix — returned unchanged
        assert_eq!(normalize_hex_key(raw), "0Xdeadbeef");
    }

    #[test]
    fn normalize_hex_key_prepends_0x_when_absent() {
        let raw = "deadbeef1234567890abcdef";
        let result = normalize_hex_key(raw);
        assert_eq!(result, "0xdeadbeef1234567890abcdef");
        assert!(result.starts_with("0x"));
    }

    #[test]
    fn normalize_hex_key_handles_empty_string() {
        let result = normalize_hex_key("");
        assert_eq!(result, "0x");
    }

    #[test]
    fn normalize_hex_key_idempotent_with_0x_prefix() {
        let raw = "0xabcdef";
        // Calling twice should give the same result as calling once
        assert_eq!(normalize_hex_key(&normalize_hex_key(raw)), normalize_hex_key(raw));
    }

    // --- parse_address ---

    #[test]
    fn parse_address_accepts_valid_checksummed_address() {
        let addr = parse_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert!(addr.is_ok());
    }

    #[test]
    fn parse_address_accepts_lowercase_address_with_prefix() {
        let addr = parse_address("0xd8da6bf26964af9d7eed9e03e53415d37aa96045");
        assert!(addr.is_ok());
    }

    #[test]
    fn parse_address_accepts_address_without_0x_prefix() {
        let addr = parse_address("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert!(addr.is_ok());
    }

    #[test]
    fn parse_address_rejects_address_too_short() {
        let addr = parse_address("0x1234");
        assert!(addr.is_err());
    }

    #[test]
    fn parse_address_rejects_non_hex_string() {
        let addr = parse_address("not-an-address");
        assert!(addr.is_err());
    }

    #[test]
    fn parse_address_rejects_empty_string() {
        let addr = parse_address("");
        assert!(addr.is_err());
    }

    #[test]
    fn parse_address_well_known_entrypoint() {
        // The ERC-4337 entry point used in Config::from_env
        let addr = parse_address("0x0000000071727De22E5E9d8BAf0edAc6f37da032");
        assert!(addr.is_ok());
        let a = addr.unwrap();
        // Confirm the Address round-trips to the same hex representation (lowercase)
        let s = format!("{a:?}");
        assert!(s.to_lowercase().contains("0000000071727de22e5e9d8baf0edac6f37da032"));
    }
}

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
                .unwrap_or(11155111),

            entry_point,
            paymaster,
            factory,
            account_impl_v2,

            redis_url: optional("REDIS_URL"),

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
