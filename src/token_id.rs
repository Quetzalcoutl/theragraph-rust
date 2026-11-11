//! `TokenId`: the one validated spelling of a marketplace token id (ticket 037).
//!
//! A marketplace id is an original id, a copy id or a token id minted by
//! `TheraFriendz` (V2 or V3). The v3 market mints copy ids
//! `(originalId << 64) | serial`, which are far past `i64`/`u64`, so no layer
//! above the chain may decode one into a machine integer. This service holds an
//! id as a canonical decimal string of a uint256 and never computes with it.
//!
//! The contract is `.wayfinder/research/037-wire-contract.md`:
//!
//! - wire (JSON to and from other services, Redis payloads): a canonical decimal
//!   STRING, `^(0|[1-9][0-9]{0,77})$` and at most 2^256 - 1;
//! - Postgres: `numeric(78,0)`. Read an id with `token_id::text`, write one with
//!   `$n::numeric` and bind the `TokenId` (it travels as text). Never decode an
//!   id column as `i64`;
//! - Rust: this type.
//!
//! Not in scope: TheraPetz / friendsz ids (small sequential counters). They stay
//! plain integers; do not use this type for them.
//!
//! # Transition rule on deserialize
//!
//! Entries written before 037 (cached recommendations in Redis and in
//! `recommendation_cache`, the genre-pool cache) carry the id as a JSON/MessagePack
//! integer. Deserializing accepts such an integer, but only when it is a safe
//! integer (0 ..= 2^53 - 1): past that a JavaScript sender has already rounded it
//! to another token, which is exactly the failure this type exists to prevent.
//! Serializing always writes a string. A float, a negative number, a bool or an
//! empty/odd string is refused.
//!
//! # Order
//!
//! Nothing in this crate sorts by id. If something must, use `Ord` on `TokenId`
//! (length first, then lexicographic: numeric order for canonical decimals) so
//! there is exactly one comparison, never a parse into a number.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 2^256 - 1, the largest id a contract can mint (78 decimal digits).
const UINT256_MAX: &str =
    "115792089237316195423570985008687907853269984665640564039457584007913129639935";

/// Largest JSON/MessagePack integer accepted in place of a string (2^53 - 1).
const MAX_SAFE_INTEGER: u64 = (1u64 << 53) - 1;

/// Why a value is not a canonical marketplace token id.
///
/// The offending input is deliberately not echoed: ids arrive from events and
/// caches, and an error string must stay short and safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenIdError {
    #[error("token id is empty")]
    Empty,
    #[error("token id must be plain decimal digits: no sign, space, point, exponent or hex prefix")]
    NotDecimal,
    #[error("token id must not have a leading zero")]
    LeadingZero,
    #[error("token id is larger than a uint256 (2^256 - 1)")]
    TooLarge,
    #[error("a JSON number may carry a token id only from 0 to 2^53 - 1; send the id as a string")]
    UnsafeNumber,
    #[error("a token id must be a canonical decimal string")]
    WrongType,
}

/// A validated canonical decimal marketplace token id (a uint256 as a string).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenId(String);

impl TokenId {
    /// Validate `s` as a canonical decimal uint256: `^(0|[1-9][0-9]{0,77})$`,
    /// at most 2^256 - 1. `"007"`, `"-1"`, `"1e3"`, `" 5"`, `""`, `"1.0"`,
    /// `"0x10"` and 2^256 are all refused.
    pub fn parse(s: &str) -> Result<TokenId, TokenIdError> {
        validate(s)?;
        Ok(TokenId(s.to_owned()))
    }

    /// The canonical decimal string. This is the wire form and the SQL bind form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The sentinel for a draft (a lazy-mint listing with no on-chain token yet).
    ///
    /// Under lazy mint `nfts.token_id` is NULL for a draft; the recommendation
    /// output type has no null, so a draft is reported as `"0"`, the same
    /// sentinel the app's `isDraftNft(tokenId)` uses (a real minted id is never
    /// 0: the contract's counter pre-increments from 0).
    pub fn draft() -> TokenId {
        TokenId("0".to_owned())
    }

    /// True for the draft sentinel `"0"`.
    pub fn is_draft(&self) -> bool {
        self.0 == "0"
    }

    /// Read an id out of an event/JSON payload value: a canonical decimal string,
    /// or (transition) a safe integer. Anything else is `None`.
    pub fn from_json(value: &serde_json::Value) -> Option<TokenId> {
        match value {
            serde_json::Value::String(s) => TokenId::parse(s).ok(),
            serde_json::Value::Number(n) => n.as_u64().and_then(|n| TokenId::from_safe_integer(n).ok()),
            _ => None,
        }
    }

    fn from_safe_integer(n: u64) -> Result<TokenId, TokenIdError> {
        if n > MAX_SAFE_INTEGER {
            return Err(TokenIdError::UnsafeNumber);
        }
        Ok(TokenId(n.to_string()))
    }
}

/// Every `u64` is a canonical decimal below 2^256: for small counters in
/// fixtures and for seeds. Not a way to feed an `i64` id in.
impl From<u64> for TokenId {
    fn from(n: u64) -> Self {
        TokenId(n.to_string())
    }
}

/// Every `u128` is a canonical decimal below 2^256.
impl From<u128> for TokenId {
    fn from(n: u128) -> Self {
        TokenId(n.to_string())
    }
}

impl From<TokenId> for String {
    fn from(id: TokenId) -> String {
        id.0
    }
}

impl AsRef<str> for TokenId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for TokenId {
    type Err = TokenIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TokenId::parse(s)
    }
}

impl fmt::Display for TokenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The ONE numeric comparison of two ids: shorter is smaller, equal length
/// compares lexicographically. Valid because canonical decimals have no
/// leading zeros, so no id is ever parsed into a number to be ordered.
impl Ord for TokenId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for TokenId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn validate(s: &str) -> Result<(), TokenIdError> {
    if s.is_empty() {
        return Err(TokenIdError::Empty);
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(TokenIdError::NotDecimal);
    }
    if s.len() > 1 && s.starts_with('0') {
        return Err(TokenIdError::LeadingZero);
    }
    // 77 digits or fewer is always below 2^256; at 78 digits the digit strings
    // have equal length, so lexicographic order is numeric order.
    match s.len().cmp(&UINT256_MAX.len()) {
        Ordering::Less => Ok(()),
        Ordering::Equal if s <= UINT256_MAX => Ok(()),
        _ => Err(TokenIdError::TooLarge),
    }
}

// ── serde: always a string out; a string or a safe integer in ────────────────

impl Serialize for TokenId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

struct TokenIdVisitor;

impl Visitor<'_> for TokenIdVisitor {
    type Value = TokenId;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a canonical decimal token id string (or an integer from 0 to 2^53 - 1)")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<TokenId, E> {
        TokenId::parse(v).map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<TokenId, E> {
        TokenId::from_safe_integer(v).map_err(E::custom)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<TokenId, E> {
        match u64::try_from(v) {
            Ok(n) => TokenId::from_safe_integer(n).map_err(E::custom),
            // A negative id never existed (the pre-037 draft sentinel was 0).
            Err(_) => Err(E::custom(TokenIdError::WrongType)),
        }
    }

    // visit_f64, visit_bool, visit_seq, ...: left at the default, which refuses
    // them. A float such as 1e3 or 1.0 is never a token id.
}

impl<'de> Deserialize<'de> for TokenId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(TokenIdVisitor)
    }
}

// ── sqlx: travels as TEXT; the SQL casts (`token_id::text`, `$n::numeric`) ───
//
// Read:  SELECT token_id::text AS token_id ...   (numeric(78,0) or bigint -> text)
// Write: ... VALUES ($1::numeric) / WHERE token_id = $1::numeric, then `.bind(&id)`.
// A raw numeric/bigint column fails to decode loudly (type mismatch) instead of
// silently truncating, which is the point.

impl sqlx::Type<sqlx::Postgres> for TokenId {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for TokenId {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<'_, sqlx::Postgres>>::encode_by_ref(&self.0.as_str(), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for TokenId {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <&str as sqlx::Decode<'r, sqlx::Postgres>>::decode(value)?;
        Ok(TokenId::parse(s)?)
    }
}

#[cfg(test)]
mod tests;
