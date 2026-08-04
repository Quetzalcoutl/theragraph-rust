//! Pipeline integration tests — NASA-1.
//!
//! Tests the full data-paths that unit tests miss. All tests run entirely
//! in-process — no live Nebula, Postgres, or Redis.
//!
//! Run: `cargo test --test pipeline_integration`

use std::sync::Arc;
use theragraph::recommendation::{
    graph_client::{DynGraphTransport, GraphClient, GraphTraversal, GraphTransport},
    schema_consts::{parse_schema_version_table, NEBULA_SCHEMA_VERSION},
};

// ── Local test doubles (test_support is cfg(test)-only, not usable here) ─────

/// No-op transport that always returns Ok("").
struct LocalNop;
impl GraphTransport for LocalNop {
    fn execute(&self, _query: &str) -> impl std::future::Future<Output = anyhow::Result<String>> + Send {
        async { Ok(String::new()) }
    }
}

/// Always-failing transport.
struct LocalAlwaysFail;
impl GraphTransport for LocalAlwaysFail {
    fn execute(&self, _query: &str) -> impl std::future::Future<Output = anyhow::Result<String>> + Send {
        async { anyhow::bail!("injected failure") }
    }
}

// ── 1. DynGraphTransport round-trip ──────────────────────────────────────────

/// `Arc<dyn GraphTraversal>` rewrapped as `GraphClient<DynGraphTransport>` must
/// still succeed — verifies the double-indirection path used by the reconciler
/// and the event processor.
#[tokio::test]
async fn dyn_transport_roundtrip_nop_succeeds() {
    let inner: Arc<dyn GraphTraversal> =
        Arc::new(GraphClient::with_transport(LocalNop));

    let outer = GraphClient::<DynGraphTransport>::from_dyn_traversal(Arc::clone(&inner));

    let result = outer.execute_write("USE theragraph; SHOW SPACES;").await;
    assert!(
        result.is_ok(),
        "DynGraphTransport round-trip via LocalNop must succeed: {:?}",
        result.err()
    );
}

/// `FailingTransport` wrapped in `DynGraphTransport` opens the outer circuit
/// breaker — verifying the circuit still protects the stack through extra
/// indirection.
#[tokio::test]
async fn dyn_transport_propagates_failures_and_opens_circuit() {
    let inner: Arc<dyn GraphTraversal> =
        Arc::new(GraphClient::with_transport(LocalAlwaysFail));

    let outer = GraphClient::<DynGraphTransport>::from_dyn_traversal(Arc::clone(&inner));

    for _ in 0..3 {
        let _ = outer.execute_write("USE theragraph;").await;
    }

    assert!(
        outer.is_circuit_open(),
        "circuit should be open after 3 failures through DynGraphTransport"
    );
}

// ── 2. Schema version constant ────────────────────────────────────────────────

#[test]
fn schema_version_constant_is_current() {
    assert!(NEBULA_SCHEMA_VERSION >= 15);
    assert_ne!(NEBULA_SCHEMA_VERSION, 0);
}

// The parser itself now lives in schema_consts.rs and has its own unit tests
// there (parse_schema_version_table_extracts_integer_from_table_row /
// _returns_none_for_empty_result) — this integration test just confirms
// main.rs's real usage of the shared function still parses the exact table
// shape nebula-console prints, exercised through the public re-export.
#[test]
fn schema_version_parser_extracts_integer_from_table_row() {
    let output = "\
+-------------------+\n\
| version           |\n\
+-------------------+\n\
| 15                |\n\
+-------------------+\n\
Got 1 rows (time spent 2345/5678 us)\n";

    assert_eq!(parse_schema_version_table(output), Some(15));
}

#[test]
fn schema_version_parser_returns_none_for_empty_result() {
    let output = "\
+-------------------+\n\
| version           |\n\
+-------------------+\n\
Empty set (time spent 123/456 us)\n";

    assert_eq!(parse_schema_version_table(output), None);
}

// ── 3. GraphClient builder + clone ───────────────────────────────────────────

#[test]
fn graph_client_clone_with_no_dlq_pool_does_not_panic() {
    let client = GraphClient::with_transport(LocalNop);
    let _cloned = client.clone();
}

// ── 4. raw_write via trait object ────────────────────────────────────────────

#[tokio::test]
async fn raw_write_via_trait_object_succeeds_on_nop() {
    let client: Arc<dyn GraphTraversal> =
        Arc::new(GraphClient::with_transport(LocalNop));

    let result = client.raw_write("USE theragraph; SHOW SPACES;").await;
    assert!(result.is_ok(), "raw_write via dyn GraphTraversal: {:?}", result.err());
}

// ── 5. Prometheus counter ────────────────────────────────────────────────────

#[test]
fn nebula_dlq_prometheus_counter_is_reachable() {
    metrics::counter!(
        "nebula_edge_write_failures_total",
        "operation" => "integration_test"
    )
    .increment(1);
}
