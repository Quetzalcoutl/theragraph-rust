// ─── Paymaster Signing Service ────────────────────────────────────────────────
//
// Signs sponsorship approvals that match TheraPaymaster.sol's
// validatePaymasterUserOp() hash scheme.
//
// paymasterAndData layout (ERC-4337 v0.7):
//   [paymaster:                     20 bytes]
//   [paymasterVerificationGasLimit: 16 bytes]
//   [paymasterPostOpGasLimit:       16 bytes]
//   [validUntil:                     6 bytes]
//   [validAfter:                     6 bytes]
//   [signature:                     65 bytes]
//   Total: 20 + 16 + 16 + 6 + 6 + 65 = 129 bytes

use alloy::{
    primitives::{Address, Bytes, B256},
    signers::local::PrivateKeySigner,
    signers::SignerSync,
};
use eyre::Result;

use super::{
    config::Config,
    hash::compute_paymaster_hash,
    types::PackedUserOperation,
};

pub struct PaymasterSigner {
    signer:                          PrivateKeySigner,
    paymaster:                       Address,
    chain_id:                        u64,
    pm_verification_gas_limit:       u128,
    pm_post_op_gas_limit:            u128,
}

impl PaymasterSigner {
    pub fn new(config: &Config) -> Result<Self> {
        let signer: PrivateKeySigner = config.private_key.parse()?;
        Ok(Self {
            signer,
            paymaster:                   config.paymaster,
            chain_id:                    config.chain_id,
            pm_verification_gas_limit:   config.gas.paymaster_verification_gas_limit as u128,
            pm_post_op_gas_limit:        config.gas.paymaster_post_op_gas_limit as u128,
        })
    }

    #[allow(dead_code)]
    pub fn address(&self) -> Address {
        self.signer.address()
    }

    /// Produce the full `paymasterAndData` field (129 bytes).
    pub fn sign_paymaster_data(
        &self,
        user_op: &PackedUserOperation,
        valid_until: u32,
        valid_after: u32,
    ) -> Result<Bytes> {
        let hash: B256 = compute_paymaster_hash(
            user_op,
            self.chain_id,
            &self.paymaster,
            valid_until,
            valid_after,
        );

        let sig = self.signer.sign_message_sync(hash.as_slice())?;
        let sig_bytes: [u8; 65] = sig.into();

        let mut out = Vec::with_capacity(129);
        out.extend_from_slice(self.paymaster.as_slice());
        out.extend_from_slice(&self.pm_verification_gas_limit.to_be_bytes());
        out.extend_from_slice(&self.pm_post_op_gas_limit.to_be_bytes());

        let vu = (valid_until as u64).to_be_bytes();
        out.extend_from_slice(&vu[2..]);

        let va = (valid_after as u64).to_be_bytes();
        out.extend_from_slice(&va[2..]);

        out.extend_from_slice(&sig_bytes);

        debug_assert_eq!(out.len(), 129, "paymasterAndData must be exactly 129 bytes");

        Ok(Bytes::from(out))
    }
}
