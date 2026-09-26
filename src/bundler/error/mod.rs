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

// ─── Batch simulation outcome ────────────────────────────────────────────────

/// Returned by `BundlerService::simulate_batch`.
#[derive(Debug)]
pub enum BatchSimOutcome {
    /// All ops passed simulation — safe to broadcast.
    Ok,
    /// Op at `index` failed with a known AA error.  The caller should remove
    /// it from the batch, send the error to its result channel, and retry
    /// simulation on the remaining ops.
    BadOp { index: usize, reason: String },
    /// RPC / infrastructure failure — abort the whole batch and retry later.
    RpcError(String),
}

// ─── Parse FailedOp from alloy error string ───────────────────────────────────
//
// alloy formats EntryPoint custom errors in two ways depending on whether the
// ABI was decoded:
//   decoded:   "FailedOp { opIndex: 1, reason: \"AA25 invalid account nonce\" }"
//   raw:       "FailedOp(1, \"AA25 ...\")"  (older alloy)
//   revert:    "execution reverted: FailedOp(opIndex: 1, reason: \"AA25...\")"
//
// Returns `Ok((op_index, reason))` on a recognised FailedOp, or
// `Err(BundlerError::Other)` when the message contains "FailedOp" but the
// index cannot be extracted (malformed / unexpected format).
// Returns `Err(BundlerError::Other("not a FailedOp"))` when "FailedOp" is
// absent entirely, so callers can cleanly distinguish the two non-happy cases.
pub(super) fn parse_failed_op(msg: &str) -> Result<(usize, String), BundlerError> {
    if !msg.contains("FailedOp") {
        return Err(BundlerError::Other("not a FailedOp".to_owned()));
    }

    // Extract opIndex ── try "opIndex: N" then positional "FailedOp(N,"
    let idx: usize = if let Some(p) = msg.find("opIndex: ") {
        let s = &msg[p + 9..];
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        if end == 0 {
            tracing::warn!("parse_failed_op: 'opIndex: ' found but no digits follow in: {msg}");
            return Err(BundlerError::Other(format!(
                "parse_failed_op: malformed opIndex in: {msg}"
            )));
        }
        s[..end].trim().parse().map_err(|_| {
            BundlerError::Other(format!("parse_failed_op: opIndex parse error in: {msg}"))
        })?
    } else if let Some(p) = msg.find("FailedOp(") {
        let s = &msg[p + 9..];
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        if end == 0 {
            tracing::warn!("parse_failed_op: 'FailedOp(' found but no digits follow in: {msg}");
            return Err(BundlerError::Other(format!(
                "parse_failed_op: malformed positional opIndex in: {msg}"
            )));
        }
        s[..end].trim().parse().map_err(|_| {
            BundlerError::Other(format!(
                "parse_failed_op: positional opIndex parse error in: {msg}"
            ))
        })?
    } else {
        return Err(BundlerError::Other(format!(
            "parse_failed_op: no opIndex anchor found in: {msg}"
        )));
    };

    // Extract reason ── try `reason: "…"` then first `"AA…` fragment
    let reason = if let Some(p) = msg.find("reason: \"") {
        let s = &msg[p + 9..];
        s[..s.find('"').unwrap_or(s.len())].to_owned()
    } else if let Some(p) = msg.find("\"AA") {
        let s = &msg[p + 1..];
        s[..s.find('"').unwrap_or(s.len())].to_owned()
    } else {
        "unknown AA error".to_owned()
    };

    Ok((idx, reason))
}


#[cfg(test)]
mod tests;
