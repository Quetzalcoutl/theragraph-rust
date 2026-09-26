use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

/// Configurable failing transport for circuit-breaker and recovery tests.
pub struct FailingTransport {
    pub fail_count: AtomicU32,
    pub fail_limit: u32, // fail this many times then succeed
}

impl FailingTransport {
    pub fn always_fail() -> Self {
        Self { fail_count: AtomicU32::new(0), fail_limit: u32::MAX }
    }
    pub fn fail_then_recover(n: u32) -> Self {
        Self { fail_count: AtomicU32::new(0), fail_limit: n }
    }
}

impl GraphTransport for FailingTransport {
    fn execute(&self, _query: &str) -> impl std::future::Future<Output = Result<String>> + Send {
        let calls = self.fail_count.fetch_add(1, Ordering::Relaxed);
        let fail_limit = self.fail_limit;
        async move {
            if calls < fail_limit {
                anyhow::bail!("injected failure #{}", calls + 1)
            }
            Ok(String::new())
        }
    }
}

/// No-op transport that always succeeds — use to test cache layer in isolation.
pub struct NopTransport;
impl GraphTransport for NopTransport {
    fn execute(&self, _query: &str) -> impl std::future::Future<Output = Result<String>> + Send {
        async { Ok(String::new()) }
    }
}

/// Records every query string passed through it — used as a golden-string
/// safety net for write_* method refactors: capture the exact nGQL a
/// method emits before changing its implementation, then assert the
/// refactored version emits byte-identical output for the same inputs.
pub struct CapturingTransport {
    pub queries: std::sync::Mutex<Vec<String>>,
}

impl CapturingTransport {
    pub fn new() -> Self {
        Self { queries: std::sync::Mutex::new(Vec::new()) }
    }

    pub fn last_query(&self) -> String {
        self.queries.lock().unwrap().last().cloned().unwrap_or_default()
    }
}

impl GraphTransport for CapturingTransport {
    fn execute(&self, query: &str) -> impl std::future::Future<Output = Result<String>> + Send {
        self.queries.lock().unwrap().push(query.to_string());
        async { Ok(String::new()) }
    }
}
