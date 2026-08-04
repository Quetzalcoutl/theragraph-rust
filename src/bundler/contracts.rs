// ─── Smart-Contract Interfaces (Alloy sol! macro) ─────────────────────────────
use alloy::sol;

sol! {
    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IEntryPoint {
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
        function handleOps(
            PackedUserOperation[] calldata ops,
            address payable beneficiary
        ) external;
        function getNonce(address sender, uint192 key)
            external view returns (uint256 nonce);
        function balanceOf(address account)
            external view returns (uint256);
        error FailedOp(uint256 opIndex, string reason);
        error FailedOpWithRevert(uint256 opIndex, string reason, bytes inner);
        error SignatureValidationFailed(address aggregator);
    }

    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IPaymaster {
        function verifyingSigner()   external view returns (address);
        function getDeposit()        external view returns (uint256);
        function sponsorshipActive() external view returns (bool);
    }

    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IFactory {
        function createAccount(address owner, uint256 salt)
            external returns (address account);
        function getAddress(address owner, uint256 salt)
            external view returns (address predicted);
    }

    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IAccount {
        function execute(
            address target,
            uint256 value,
            bytes calldata data
        ) external;
        function executeBatch(
            address[] calldata targets,
            uint256[] calldata values,
            bytes[]   calldata datas
        ) external;
        function supportsInterface(bytes4 interfaceId)
            external view returns (bool);
    }
}
