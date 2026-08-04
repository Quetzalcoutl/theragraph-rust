/// Validated, normalized Ethereum address.
///
/// Invariant: always lowercase, always `0x` + 40 hex chars.
/// Construct via [`EthAddress::try_from`] or [`EthAddress::from_str`].
/// Nothing in this crate should call `.to_lowercase()` on a raw address
/// string — use this type at the seam instead.
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct EthAddress(String);

#[derive(Debug, thiserror::Error)]
pub enum AddressError {
    #[error("invalid Ethereum address: expected 0x + 40 hex chars, got {0:?}")]
    Invalid(String),
}

#[allow(dead_code)]
impl EthAddress {
    /// Inner normalized string reference.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Normalize a raw string without length check (for internal use where
    /// addresses have already been validated upstream).
    pub fn normalize_unchecked(raw: &str) -> String {
        raw.to_lowercase()
    }
}

impl FromStr for EthAddress {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_lowercase();
        if lower.len() == 42
            && lower.starts_with("0x")
            && lower[2..].chars().all(|c| c.is_ascii_hexdigit())
        {
            Ok(Self(lower))
        } else {
            Err(AddressError::Invalid(s.to_owned()))
        }
    }
}

impl TryFrom<&str> for EthAddress {
    type Error = AddressError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl TryFrom<String> for EthAddress {
    type Error = AddressError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.as_str().parse()
    }
}

impl fmt::Display for EthAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for EthAddress {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Typed cache key constructors — thin delegates to the canonical
/// `crate::recommendation::cache::CacheKey` so address.rs callers
/// don't need to import the recommendation module directly.
///
/// The recommendation module owns the PREFIX_* constants; these wrappers
/// simply forward calls so both call sites stay in sync automatically.
#[allow(dead_code)]
pub struct CacheKey;

#[allow(dead_code)]
impl CacheKey {
    pub fn user_prefs(addr: &str) -> String {
        crate::recommendation::cache::CacheKey::prefs(addr)
    }

    pub fn user_recommendations(addr: &str, feed_type: &str) -> String {
        crate::recommendation::cache::CacheKey::recs(addr, feed_type)
    }

    pub fn user_following(addr: &str) -> String {
        crate::recommendation::cache::CacheKey::following(addr)
    }

    pub fn user_seen(addr: &str) -> String {
        crate::recommendation::cache::CacheKey::seen(addr)
    }

    pub fn nft_features(nft_id: &str) -> String {
        crate::recommendation::cache::CacheKey::features(nft_id)
    }

    pub fn nebula_query(query_hash: &str) -> String {
        crate::recommendation::cache::CacheKey::nebula(query_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_mixed_case() {
        let addr: EthAddress = "0xABCD1234abcd1234ABCD1234abcd1234ABCD1234".parse().unwrap();
        assert_eq!(addr.as_str(), "0xabcd1234abcd1234abcd1234abcd1234abcd1234");
    }

    #[test]
    fn rejects_short_address() {
        assert!("0x123".parse::<EthAddress>().is_err());
    }

    #[test]
    fn rejects_no_prefix() {
        assert!("abcd1234abcd1234abcd1234abcd1234abcd1234ab".parse::<EthAddress>().is_err());
    }

    #[test]
    fn cache_keys_are_lowercase() {
        let key = CacheKey::user_prefs("0xABCD1234abcd1234ABCD1234abcd1234ABCD1234");
        assert!(key.contains("0xabcd"));
        assert!(!key.contains("ABCD"));
    }
}
