// ─── ERC-4337 v0.7 Domain Types ───────────────────────────────────────────────
use alloy::primitives::{Address, Bytes, FixedBytes, B256, U256};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct PackedUserOperation {
    pub sender: Address,
    pub nonce: U256,
    pub init_code: Bytes,
    pub call_data: Bytes,
    pub account_gas_limits: FixedBytes<32>,
    pub pre_verification_gas: U256,
    pub gas_fees: FixedBytes<32>,
    pub paymaster_and_data: Bytes,
    pub signature: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub target: Address,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<HexU256>,
    pub data: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerializedUserOp {
    pub sender: Address,
    pub nonce: HexU256,
    pub init_code: Bytes,
    pub call_data: Bytes,
    pub account_gas_limits: FixedBytes<32>,
    pub pre_verification_gas: HexU256,
    pub gas_fees: FixedBytes<32>,
    pub paymaster_and_data: Bytes,
    pub signature: Bytes,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SponsorRequest {
    pub sender: Option<Address>,
    pub owner_address: Option<Address>,
    pub calls: Vec<Call>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SponsorResponse {
    pub user_op: SerializedUserOp,
    pub user_op_hash: B256,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest {
    pub user_op: SerializedUserOp,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResponse {
    pub user_op_hash: B256,
    pub tx_hash: B256,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountResponse {
    pub owner: Address,
    pub smart_account: Address,
    pub deployed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub tx_hash: B256,
    pub block_number: Option<u64>,
    pub success: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    pub fn ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self { jsonrpc: "2.0", id, result: Some(result), error: None }
    }

    pub fn err(id: Option<serde_json::Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError { code, message: message.into(), data: None }),
        }
    }
}

impl From<&PackedUserOperation> for SerializedUserOp {
    fn from(op: &PackedUserOperation) -> Self {
        Self {
            sender: op.sender,
            nonce: HexU256(op.nonce),
            init_code: op.init_code.clone(),
            call_data: op.call_data.clone(),
            account_gas_limits: op.account_gas_limits,
            pre_verification_gas: HexU256(op.pre_verification_gas),
            gas_fees: op.gas_fees,
            paymaster_and_data: op.paymaster_and_data.clone(),
            signature: op.signature.clone(),
        }
    }
}

impl From<SerializedUserOp> for PackedUserOperation {
    fn from(s: SerializedUserOp) -> Self {
        Self {
            sender: s.sender,
            nonce: s.nonce.0,
            init_code: s.init_code,
            call_data: s.call_data,
            account_gas_limits: s.account_gas_limits,
            pre_verification_gas: s.pre_verification_gas.0,
            gas_fees: s.gas_fees,
            paymaster_and_data: s.paymaster_and_data,
            signature: s.signature,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexU256(pub U256);

impl Serialize for HexU256 {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&format!("{:#x}", self.0))
    }
}

impl<'de> Deserialize<'de> for HexU256 {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        let stripped = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(&s);
        let n = U256::from_str_radix(stripped, 16).map_err(serde::de::Error::custom)?;
        Ok(HexU256(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, Bytes, FixedBytes, U256};

    // ── JsonRpcResponse::ok ────────────────────────────────────────────────────

    #[test]
    fn json_rpc_ok_has_result_no_error() {
        let resp = JsonRpcResponse::ok(Some(serde_json::json!(1)), serde_json::json!("pong"));
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Some(serde_json::json!(1)));
        assert_eq!(resp.result, Some(serde_json::json!("pong")));
        assert!(resp.error.is_none());
    }

    #[test]
    fn json_rpc_ok_null_id() {
        let resp = JsonRpcResponse::ok(None, serde_json::json!(42));
        assert!(resp.id.is_none());
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    // ── JsonRpcResponse::err ───────────────────────────────────────────────────

    #[test]
    fn json_rpc_err_has_error_no_result() {
        let resp = JsonRpcResponse::err(Some(serde_json::json!(2)), -32600, "Invalid Request");
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Some(serde_json::json!(2)));
        assert!(resp.result.is_none());
        let err = resp.error.expect("error field must be set");
        assert_eq!(err.code, -32600);
        assert_eq!(err.message, "Invalid Request");
        assert!(err.data.is_none());
    }

    #[test]
    fn json_rpc_err_accepts_string_message() {
        let resp = JsonRpcResponse::err(None, -32000, String::from("server error"));
        let err = resp.error.unwrap();
        assert_eq!(err.message, "server error");
    }

    // ── HexU256 serde ─────────────────────────────────────────────────────────

    #[test]
    fn hex_u256_serialize_zero() {
        let s = serde_json::to_string(&HexU256(U256::ZERO)).unwrap();
        // alloy formats U256::ZERO as "0x0"
        assert_eq!(s, "\"0x0\"");
    }

    #[test]
    fn hex_u256_serialize_nonzero() {
        let s = serde_json::to_string(&HexU256(U256::from(255u64))).unwrap();
        assert_eq!(s, "\"0xff\"");
    }

    #[test]
    fn hex_u256_deserialize_lowercase() {
        let v: HexU256 = serde_json::from_str("\"0x1\"").unwrap();
        assert_eq!(v, HexU256(U256::from(1u64)));
    }

    #[test]
    fn hex_u256_deserialize_uppercase_prefix() {
        let v: HexU256 = serde_json::from_str("\"0XFF\"").unwrap();
        assert_eq!(v, HexU256(U256::from(255u64)));
    }

    #[test]
    fn hex_u256_deserialize_no_prefix() {
        let v: HexU256 = serde_json::from_str("\"ff\"").unwrap();
        assert_eq!(v, HexU256(U256::from(255u64)));
    }

    #[test]
    fn hex_u256_roundtrip_via_json() {
        let original = HexU256(U256::from(0xdeadbeef_u64));
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: HexU256 = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, original);
    }

    #[test]
    fn hex_u256_roundtrip_large_value() {
        // A value that occupies more than one limb
        let big = U256::from(u128::MAX);
        let original = HexU256(big);
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: HexU256 = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, original);
    }

    // ── PackedUserOperation ↔ SerializedUserOp round-trip ─────────────────────

    fn zero_op() -> PackedUserOperation {
        PackedUserOperation {
            sender: Address::ZERO,
            nonce: U256::ZERO,
            init_code: Bytes::new(),
            call_data: Bytes::new(),
            account_gas_limits: FixedBytes::ZERO,
            pre_verification_gas: U256::ZERO,
            gas_fees: FixedBytes::ZERO,
            paymaster_and_data: Bytes::new(),
            signature: Bytes::new(),
        }
    }

    #[test]
    fn packed_user_op_round_trip_zero() {
        let op = zero_op();
        let serialized = SerializedUserOp::from(&op);
        let back = PackedUserOperation::from(serialized);

        assert_eq!(back.sender, op.sender);
        assert_eq!(back.nonce, op.nonce);
        assert_eq!(back.init_code, op.init_code);
        assert_eq!(back.call_data, op.call_data);
        assert_eq!(back.account_gas_limits, op.account_gas_limits);
        assert_eq!(back.pre_verification_gas, op.pre_verification_gas);
        assert_eq!(back.gas_fees, op.gas_fees);
        assert_eq!(back.paymaster_and_data, op.paymaster_and_data);
        assert_eq!(back.signature, op.signature);
    }

    #[test]
    fn packed_user_op_round_trip_nonzero() {
        let mut op = zero_op();
        op.sender = Address::repeat_byte(0xab);
        op.nonce = U256::from(7u64);
        op.call_data = Bytes::from(vec![0x01, 0x02, 0x03]);
        op.pre_verification_gas = U256::from(21_000u64);
        op.signature = Bytes::from(vec![0xff; 65]);

        let serialized = SerializedUserOp::from(&op);
        let back = PackedUserOperation::from(serialized);

        assert_eq!(back.sender, op.sender);
        assert_eq!(back.nonce, op.nonce);
        assert_eq!(back.call_data, op.call_data);
        assert_eq!(back.pre_verification_gas, op.pre_verification_gas);
        assert_eq!(back.signature, op.signature);
    }

    #[test]
    fn serialized_user_op_nonce_is_hex_encoded() {
        let mut op = zero_op();
        op.nonce = U256::from(16u64); // 0x10
        let serialized = SerializedUserOp::from(&op);
        // HexU256 wraps the nonce — confirm it round-trips through JSON correctly
        let json_str = serde_json::to_string(&serialized).unwrap();
        assert!(json_str.contains("0x10"), "nonce should appear as 0x10 in JSON, got: {json_str}");
    }
}
