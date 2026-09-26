// ─── UserOp ABI Encoding ───────────────────────────────────────────────────────
//
// Pure functions that convert domain types into ABI-encoded bytes or Alloy
// sol-types.  No provider access, no async — everything here is deterministic
// and fully unit-testable without a live node.
//
// Why separate from service.rs: wrong ABI encoding breaks every transaction
// silently (simulation may pass; on-chain execution reverts or sends to the
// wrong target).  Having these as named, exported, tested functions makes
// encoding bugs findable before they reach a live chain.
//
// Pattern mirrors gas.rs: pure free functions, explicit parameters, no &self.

use alloy::{
    primitives::{Address, Bytes, U256},
    sol_types::SolCall,
};

use super::{
    contracts::{IAccount, IEntryPoint, IFactory},
    types::{Call, PackedUserOperation},
};

/// Encode the callData field for a UserOp.
///
/// Single call → `IAccount.execute(target, value, data)` (cheaper gas).
/// Multiple calls → `IAccount.executeBatch(targets[], values[], datas[])`.
///
/// # Panics
/// Panics if `calls` is empty — callers must validate before building a UserOp.
pub fn encode_call_data(calls: &[Call]) -> Bytes {
    assert!(!calls.is_empty(), "encode_call_data: calls must not be empty");
    if calls.len() == 1 {
        let c = &calls[0];
        Bytes::from(
            IAccount::executeCall {
                target: c.target,
                value:  c.value.map(|v| v.0).unwrap_or(U256::ZERO),
                data:   c.data.clone(),
            }
            .abi_encode(),
        )
    } else {
        Bytes::from(
            IAccount::executeBatchCall {
                targets: calls.iter().map(|c| c.target).collect(),
                values:  calls
                    .iter()
                    .map(|c| c.value.map(|v| v.0).unwrap_or(U256::ZERO))
                    .collect(),
                datas: calls.iter().map(|c| c.data.clone()).collect(),
            }
            .abi_encode(),
        )
    }
}

/// Encode the initCode field for first-time account deployment.
///
/// ERC-4337 §6.1: initCode = factory_address (20 bytes) ++ factory_calldata.
/// The factory calldata is `createAccount(owner, salt=0)` ABI-encoded.
///
/// **salt is always 0** — TheraFriendz factory design gives every owner a single
/// deterministic counterfactual address (`getAddress(owner, 0)`).  If the factory
/// ever supports multiple accounts per owner this function will need a `salt`
/// parameter; until then, hardcoding 0 ensures on-chain and off-chain address
/// derivation agree.
///
/// Callers must check `is_deployed` first — a non-empty initCode for an already-
/// deployed account causes the EntryPoint to revert with AA10.
pub fn encode_init_code(factory: Address, owner: Address) -> Bytes {
    let calldata = IFactory::createAccountCall { owner, salt: U256::ZERO }.abi_encode();
    let mut out = Vec::with_capacity(20 + calldata.len());
    out.extend_from_slice(factory.as_slice());
    out.extend_from_slice(&calldata);
    Bytes::from(out)
}

/// Map a domain `PackedUserOperation` to the Alloy sol-generated EntryPoint type.
///
/// Field mapping is one-to-one.  Any name mismatch here means the wrong UserOp
/// reaches the EntryPoint — simulation may pass but on-chain execution reverts.
pub fn to_entry_point_op(op: &PackedUserOperation) -> IEntryPoint::PackedUserOperation {
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{FixedBytes, address};

    use super::super::types::HexU256;

    fn dead() -> Address { address!("DeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF") }
    fn zero() -> Address { Address::ZERO }

    fn call(target: Address, value: Option<U256>, data: &[u8]) -> Call {
        Call { target, value: value.map(HexU256), data: Bytes::from(data.to_vec()) }
    }

    // ── encode_call_data ──────────────────────────────────────────────────────
    //
    // Selectors are fixed by the Solidity ABI spec and must never change:
    //   execute(address,uint256,bytes)          → 0xb61d27f6
    //   executeBatch(address[],uint256[],bytes[]) → 0x47e1da2a
    // These values come from Alloy's sol! macro output verified at build time.

    #[test]
    fn single_call_has_known_execute_selector() {
        let encoded = encode_call_data(&[call(dead(), None, b"")]);
        assert!(encoded.len() >= 4);
        assert_eq!(
            &encoded[..4], &[0xb6, 0x1d, 0x27, 0xf6],
            "execute(address,uint256,bytes) selector must be 0xb61d27f6"
        );
    }

    #[test]
    fn batch_call_has_known_execute_batch_selector() {
        let encoded = encode_call_data(&[call(dead(), None, b"x"), call(zero(), None, b"y")]);
        assert!(encoded.len() >= 4);
        assert_eq!(
            &encoded[..4], &[0x47, 0xe1, 0xda, 0x2a],
            "executeBatch(address[],uint256[],bytes[]) selector must be 0x47e1da2a"
        );
    }

    #[test]
    fn value_none_encodes_same_as_zero() {
        let with_none = encode_call_data(&[call(dead(), None, b"")]);
        let with_zero = encode_call_data(&[call(dead(), Some(U256::ZERO), b"")]);
        assert_eq!(with_none, with_zero);
    }

    #[test]
    fn non_zero_value_differs_from_zero() {
        let no_val  = encode_call_data(&[call(dead(), None, b"")]);
        let one_wei = encode_call_data(&[call(dead(), Some(U256::from(1u64)), b"")]);
        assert_ne!(no_val, one_wei);
    }

    #[test]
    fn same_calls_produce_identical_encoding() {
        let c = call(dead(), Some(U256::from(42u64)), b"deadbeef");
        assert_eq!(encode_call_data(&[c.clone()]), encode_call_data(&[c]));
    }

    // ── encode_init_code ──────────────────────────────────────────────────────

    #[test]
    fn init_code_starts_with_factory_address() {
        let factory = dead();
        let init    = encode_init_code(factory, zero());
        assert!(init.len() >= 20);
        assert_eq!(&init[..20], factory.as_slice());
    }

    #[test]
    fn init_code_length_is_factory_plus_calldata() {
        // createAccount(address,uint256): 4-byte selector + 32 + 32 = 68 bytes
        let init = encode_init_code(dead(), zero());
        assert_eq!(init.len(), 20 + 68);
    }

    #[test]
    fn different_owners_produce_different_init_codes() {
        let a = encode_init_code(dead(), zero());
        let b = encode_init_code(dead(), dead());
        assert_ne!(a, b);
    }

    #[test]
    fn different_factories_produce_different_init_codes() {
        let a = encode_init_code(zero(), zero());
        let b = encode_init_code(dead(), zero());
        assert_ne!(a, b);
    }

    // ── to_entry_point_op ────────────────────────────────────────────────────

    #[test]
    fn to_entry_point_op_maps_all_fields() {
        let op = PackedUserOperation {
            sender:               dead(),
            nonce:                U256::from(7u64),
            init_code:            Bytes::from(vec![1, 2, 3]),
            call_data:            Bytes::from(vec![4, 5, 6]),
            account_gas_limits:   FixedBytes([0xAA; 32]),
            pre_verification_gas: U256::from(42u64),
            gas_fees:             FixedBytes([0xBB; 32]),
            paymaster_and_data:   Bytes::from(vec![7, 8]),
            signature:            Bytes::from(vec![9]),
        };
        let sol = to_entry_point_op(&op);
        assert_eq!(sol.sender,              op.sender);
        assert_eq!(sol.nonce,               op.nonce);
        assert_eq!(sol.initCode,            op.init_code);
        assert_eq!(sol.callData,            op.call_data);
        assert_eq!(sol.accountGasLimits,    op.account_gas_limits);
        assert_eq!(sol.preVerificationGas,  op.pre_verification_gas);
        assert_eq!(sol.gasFees,             op.gas_fees);
        assert_eq!(sol.paymasterAndData,    op.paymaster_and_data);
        assert_eq!(sol.signature,           op.signature);
    }
}
