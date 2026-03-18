// ─── Smart-Contract Interfaces (Alloy sol! macro) ─────────────────────────────
//
// The sol! macro generates fully type-safe Rust bindings from inline Solidity.
// Using it instead of raw ABI JSON gives us:
//   • Compile-time ABI verification
//   • Zero-cost encoding / decoding for CallData and logs
//   • Ergonomic contract call syntax via #[sol(rpc)]
//
// Architecture note (Diego B. / Yalantis):
//   Keep ABI definitions in one place.  Bundler logic imports individual call
//   types; nothing reaches for `abi.ts`-style raw JSON at runtime.

use alloy::sol;

sol! {
    // ─── ERC-4337 EntryPoint v0.7 ─────────────────────────────────────────────

    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IEntryPoint {
        /// ERC-4337 v0.7 packed user operation.
        struct PackedUserOperation {
            address sender;
            uint256 nonce;
            bytes   initCode;
            bytes   callData;
            bytes32 accountGasLimits;
            uint256 preVerificationGas;
            bytes32 gasFees;
            bytes   paymasterAndData;
            bytes   signature;
        }

        /// Submit a batch of user operations.
        function handleOps(
            PackedUserOperation[] calldata ops,
            address payable beneficiary
        ) external;

        /// Get the next valid nonce for a sender and key.
        function getNonce(address sender, uint192 key)
            external view returns (uint256 nonce);

        /// Get the deposited stake balance for an account.
        function balanceOf(address account)
            external view returns (uint256);

        // ── Errors (needed so Alloy can decode revert data) ──────────────────

        error FailedOp(uint256 opIndex, string reason);
        error FailedOpWithRevert(uint256 opIndex, string reason, bytes inner);
        error SignatureValidationFailed(address aggregator);
    }

    // ─── Thera Paymaster ──────────────────────────────────────────────────────

    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IPaymaster {
        function verifyingSigner()   external view returns (address);
        function getDeposit()        external view returns (uint256);
        function sponsorshipActive() external view returns (bool);
    }

    // ─── Thera Account Factory ────────────────────────────────────────────────

    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IFactory {
        function createAccount(address owner, uint256 salt)
            external returns (address account);

        function getAddress(address owner, uint256 salt)
            external view returns (address predicted);
    }

    // ─── Thera Smart Account ──────────────────────────────────────────────────

    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IAccount {
        /// Execute a single call.
        function execute(
            address target,
            uint256 value,
            bytes calldata data
        ) external;

        /// Execute a batch of calls.
        function executeBatch(
            address[] calldata targets,
            uint256[] calldata values,
            bytes[]   calldata datas
        ) external;

        /// ERC-165 — used to detect v2 (adds onERC721Received).
        function supportsInterface(bytes4 interfaceId)
            external view returns (bool);
    }
}
