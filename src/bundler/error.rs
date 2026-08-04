// ─── Bundler Error Types ──────────────────────────────────────────────────────

/// Typed errors produced by the ERC-4337 bundler.
///
/// `FailedOp` carries the structured data alloy emits from the EntryPoint
/// `FailedOp(opIndex, reason)` custom error, making the call site independent
/// of alloy's error message format.  `Other` is a fallback for anything that
/// does not parse as a known EntryPoint error.
#[derive(Debug)]
pub enum BundlerError {
    /// The EntryPoint rejected the UserOp at `op_index` with `reason`.
    #[allow(dead_code)]
    FailedOp { op_index: usize, reason: String },
    /// Any other bundler-level error (infrastructure, RPC, etc.).
    Other(String),
}

impl std::fmt::Display for BundlerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BundlerError::FailedOp { op_index, reason } => {
                write!(f, "FailedOp(opIndex: {op_index}, reason: \"{reason}\")")
            }
            BundlerError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for BundlerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_op_display_contains_failed_op_and_reason() {
        let err = BundlerError::FailedOp {
            op_index: 0,
            reason: "AA21 didn't pay prefund".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("FailedOp"), "expected 'FailedOp' in: {s}");
        assert!(s.contains("AA21 didn't pay prefund"), "expected reason in: {s}");
    }

    #[test]
    fn other_display_equals_message() {
        let msg = "rpc connection failed";
        let err = BundlerError::Other(msg.to_string());
        assert_eq!(err.to_string(), msg);
    }

    #[test]
    fn other_implements_std_error() {
        let err = BundlerError::Other("boom".to_string());
        // Calling source() on a bare Error impl returns None — this must not panic.
        let _source = std::error::Error::source(&err);
    }
}
