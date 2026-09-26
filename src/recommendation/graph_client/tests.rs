use super::*;
use std::sync::atomic::Ordering;
use super::circuit_breaker::CIRCUIT_OPEN_THRESHOLD;
use super::test_support::{CapturingTransport, FailingTransport};
use crate::recommendation::graph_transport::parse_nebula_table;
use crate::recommendation::schema_consts::{
    vid_user, vid_post, comment_rank,
    SPACE_THERAGRAPH,
    EDGE_FOLLOWS, EDGE_LIKES, EDGE_PURCHASES, EDGE_RECOMMENDED_TO,
    EDGE_COMMENTS_ON, EDGE_BOOKMARKED,
    PROP_WEIGHT, PROP_EVENT_ID, PROP_FOLLOWED_AT, PROP_LIKED_AT, PROP_PURCHASED_AT,
    PROP_COMMENTED_AT, PROP_BOOKMARKED_AT, PROP_REACTION_TYPE, PROP_COMMENT_TEXT, PROP_SERVED,
    ensure_user_vertex_nql, ensure_post_vertex_nql,
};

// ── Golden-string safety net for the write_edge/delete_edge consolidation ──
// No cache is configured on these GraphClients, so vertex_bloom_check always
// returns (false, false) and every upsert clause is always emitted — the
// generated query is a pure deterministic function of the inputs below.
// These lock in the exact nGQL each write_* method produced BEFORE the
// write_edge/delete_edge helper existed; the refactor must keep them
// byte-identical.

const ADDR_A: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ADDR_B: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const POST_ID: &str = "post-uuid-1234";
const EVENT_ID: &str = "0xdeadbeef";

#[tokio::test]
async fn golden_write_follows_edge() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.write_follows_edge(ADDR_A, ADDR_B, EVENT_ID).await;
    let expected = format!(
        "USE {space};\n{upsert_fwr}\n{upsert_fwe}\nINSERT EDGE IF NOT EXISTS {e_follows}({p_eid}, {p_followed_at}, {p_weight}) VALUES \"{fwr_vid}\" -> \"{fwe_vid}\":(\"{eid}\", now(), 1.0);",
        space = SPACE_THERAGRAPH,
        upsert_fwr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
        upsert_fwe = ensure_user_vertex_nql(&vid_user(ADDR_B), ADDR_B),
        e_follows = EDGE_FOLLOWS,
        p_eid = PROP_EVENT_ID,
        p_followed_at = PROP_FOLLOWED_AT,
        p_weight = PROP_WEIGHT,
        fwr_vid = vid_user(ADDR_A),
        fwe_vid = vid_user(ADDR_B),
        eid = EVENT_ID,
    );
    assert_eq!(transport.last_query(), expected);
}

#[tokio::test]
async fn golden_delete_follows_edge() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.delete_follows_edge(ADDR_A, ADDR_B).await;
    let expected = format!(
        "USE {space};\nDELETE EDGE {e_follows} \"{fwr_vid}\" -> \"{fwe_vid}\";",
        space = SPACE_THERAGRAPH,
        e_follows = EDGE_FOLLOWS,
        fwr_vid = vid_user(ADDR_A),
        fwe_vid = vid_user(ADDR_B),
    );
    assert_eq!(transport.last_query(), expected);
}

#[tokio::test]
async fn golden_write_comments_on() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.write_comments_on(ADDR_A, POST_ID, EVENT_ID, "nice post").await;
    let rank = comment_rank(EVENT_ID);
    let expected = format!(
        "USE {space};\n{upsert_cmtr}\n{upsert_pid}\nINSERT EDGE {e_comments_on}({p_eid}, {p_comment_text}, {p_commented_at}) VALUES \"{cmtr_vid}\" -> \"{pid_vid}\"@{rank}:(\"{eid}\", \"{preview}\", now());",
        space = SPACE_THERAGRAPH,
        upsert_cmtr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
        upsert_pid = ensure_post_vertex_nql(&vid_post(POST_ID), POST_ID),
        cmtr_vid = vid_user(ADDR_A),
        pid_vid = vid_post(POST_ID),
        e_comments_on = EDGE_COMMENTS_ON,
        rank = rank,
        eid = EVENT_ID,
        preview = "nice post",
        p_eid = PROP_EVENT_ID,
        p_comment_text = PROP_COMMENT_TEXT,
        p_commented_at = PROP_COMMENTED_AT,
    );
    assert_eq!(transport.last_query(), expected);
}

#[tokio::test]
async fn golden_write_likes_edge() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.write_likes_edge(ADDR_A, POST_ID, EVENT_ID, "like").await;
    let weight = map_reaction_weight("like");
    let expected = format!(
        "USE {space};\n{upsert_lkr}\n{upsert_pid}\nINSERT EDGE IF NOT EXISTS {e_likes}({p_eid}, {p_liked_at}, {p_rt}, {p_weight}) VALUES \"{lkr_vid}\" -> \"{pid_vid}\":(\"{eid}\", now(), \"{rt}\", {wt});",
        space = SPACE_THERAGRAPH,
        upsert_lkr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
        upsert_pid = ensure_post_vertex_nql(&vid_post(POST_ID), POST_ID),
        lkr_vid = vid_user(ADDR_A),
        pid_vid = vid_post(POST_ID),
        e_likes = EDGE_LIKES,
        eid = EVENT_ID,
        rt = "like",
        wt = weight,
        p_eid = PROP_EVENT_ID,
        p_liked_at = PROP_LIKED_AT,
        p_rt = PROP_REACTION_TYPE,
        p_weight = PROP_WEIGHT,
    );
    assert_eq!(transport.last_query(), expected);
}

#[tokio::test]
async fn golden_write_purchases_edge() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.write_purchases_edge(ADDR_A, POST_ID, EVENT_ID).await;
    let expected = format!(
        "USE {space};\n{upsert_usr}\n{upsert_pid}\nINSERT EDGE {e_purchases}({p_eid}, {p_purchased_at}, {p_weight}) VALUES \"{buyer_vid}\" -> \"{pid_vid}\":(\"{eid}\", now(), 2.0);",
        space = SPACE_THERAGRAPH,
        upsert_usr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
        upsert_pid = ensure_post_vertex_nql(&vid_post(POST_ID), POST_ID),
        buyer_vid = vid_user(ADDR_A),
        pid_vid = vid_post(POST_ID),
        e_purchases = EDGE_PURCHASES,
        eid = EVENT_ID,
        p_eid = PROP_EVENT_ID,
        p_purchased_at = PROP_PURCHASED_AT,
        p_weight = PROP_WEIGHT,
    );
    assert_eq!(transport.last_query(), expected);
}

#[tokio::test]
async fn golden_write_bookmark_edge() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.write_bookmark_edge(ADDR_A, POST_ID, EVENT_ID).await;
    let expected = format!(
        "USE {space};\n{upsert_usr}\n{upsert_pid}\nINSERT EDGE IF NOT EXISTS {e_bm}({p_eid}, {p_bm_at}) VALUES \"{usr_vid}\" -> \"{pid_vid}\":(\"{eid}\", now());",
        space = SPACE_THERAGRAPH,
        upsert_usr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
        upsert_pid = ensure_post_vertex_nql(&vid_post(POST_ID), POST_ID),
        usr_vid = vid_user(ADDR_A),
        pid_vid = vid_post(POST_ID),
        e_bm = EDGE_BOOKMARKED,
        p_eid = PROP_EVENT_ID,
        p_bm_at = PROP_BOOKMARKED_AT,
        eid = EVENT_ID,
    );
    assert_eq!(transport.last_query(), expected);
}

#[tokio::test]
async fn golden_delete_bookmark_edge() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.delete_bookmark_edge(ADDR_A, POST_ID).await;
    let expected = format!(
        "USE {space};\nDELETE EDGE {e_bm} \"{usr_vid}\" -> \"{pid_vid}\";",
        space = SPACE_THERAGRAPH,
        e_bm = EDGE_BOOKMARKED,
        usr_vid = vid_user(ADDR_A),
        pid_vid = vid_post(POST_ID),
    );
    assert_eq!(transport.last_query(), expected);
}

// ── Regression tests: UPDATE EDGE ... WHEN must carry an explicit YIELD ──
// Nebula rejects `UPDATE EDGE ... WHEN <cond>;` with no YIELD clause as
// `SemanticError: Missing yield clause` — a real production failure that
// opened the write circuit breaker after 6-7 consecutive failures on
// every recommendation-serving write (write_recommended_to_batch is
// called on every feed request). The other two UPDATE-EDGE-WHEN call
// sites had the identical gap; they just hadn't been observed failing
// yet since they only fire on click/purchase, not every feed request.

#[tokio::test]
async fn write_recommended_to_batch_includes_yield_clause() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.write_recommended_to_batch(ADDR_A, &[(POST_ID.into(), 1.0)]).await.unwrap();
    let query = transport.last_query();
    assert!(query.contains("UPDATE EDGE"), "expected an UPDATE EDGE statement: {query}");
    assert!(
        query.contains(&format!("YIELD {EDGE_RECOMMENDED_TO}.{PROP_SERVED} AS served")),
        "UPDATE EDGE ... WHEN needs an explicit YIELD or Nebula rejects it: {query}"
    );
}

#[tokio::test]
async fn mark_recommendation_served_includes_yield_clause() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.mark_recommendation_served(ADDR_A, POST_ID).await;
    let query = transport.last_query();
    assert!(
        query.contains(&format!("YIELD {EDGE_RECOMMENDED_TO}.{PROP_SERVED} AS served")),
        "UPDATE EDGE ... WHEN needs an explicit YIELD or Nebula rejects it: {query}"
    );
}

#[tokio::test]
async fn mark_recommendations_served_batch_includes_yield_clause() {
    let gc = GraphClient::with_transport(CapturingTransport::new());
    let transport = Arc::clone(&gc.transport);
    gc.mark_recommendations_served_batch(ADDR_A, &[POST_ID.to_owned()]).await;
    let query = transport.last_query();
    assert!(
        query.contains(&format!("YIELD {EDGE_RECOMMENDED_TO}.{PROP_SERVED} AS served")),
        "UPDATE EDGE ... WHEN needs an explicit YIELD or Nebula rejects it: {query}"
    );
}

// RS-09: circuit opens after CIRCUIT_OPEN_THRESHOLD consecutive failures.
#[tokio::test]
async fn circuit_opens_after_threshold_failures() {
    let gc = GraphClient::with_transport(FailingTransport::always_fail());
    for _ in 0..CIRCUIT_OPEN_THRESHOLD {
        let _ = gc.execute_query("USE theragraph;").await;
    }
    assert!(
        gc.read_cb.circuit_open.load(Ordering::Relaxed),
        "circuit should be open after {CIRCUIT_OPEN_THRESHOLD} failures"
    );
}

// RS-09: circuit stays closed when failures are below threshold.
#[tokio::test]
async fn circuit_stays_closed_below_threshold() {
    let gc = GraphClient::with_transport(FailingTransport::fail_then_recover(
        CIRCUIT_OPEN_THRESHOLD - 1,
    ));
    for _ in 0..(CIRCUIT_OPEN_THRESHOLD - 1) {
        let _ = gc.execute_query("USE theragraph;").await;
    }
    assert!(
        !gc.read_cb.circuit_open.load(Ordering::Relaxed),
        "circuit should still be closed below threshold"
    );
}

// RS-09: consecutive_failures resets to 0 after a successful call.
#[tokio::test]
async fn success_resets_consecutive_failures() {
    // fail THRESHOLD-1 times, then succeed
    let gc = GraphClient::with_transport(FailingTransport::fail_then_recover(
        CIRCUIT_OPEN_THRESHOLD - 1,
    ));
    for _ in 0..(CIRCUIT_OPEN_THRESHOLD - 1) {
        let _ = gc.execute_query("USE theragraph;").await;
    }
    // should succeed now
    let result = gc.execute_query("USE theragraph;").await;
    assert!(result.is_ok(), "expected success after recovery");
    assert_eq!(
        gc.read_cb.consecutive_failures.load(Ordering::Relaxed),
        0,
        "consecutive_failures should reset to 0 on success"
    );
}

// RS-09: open circuit returns Err immediately without hitting the transport.
#[tokio::test]
async fn open_circuit_returns_err_without_transport_call() {
    let transport = FailingTransport::always_fail();
    let gc = GraphClient::with_transport(transport);

    // Force circuit open
    for _ in 0..CIRCUIT_OPEN_THRESHOLD {
        let _ = gc.execute_query("USE theragraph;").await;
    }
    assert!(gc.read_cb.circuit_open.load(Ordering::Relaxed));

    // Force last_opened_at to the recent past so the 30-second cooldown blocks
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    gc.read_cb.last_opened_at.store(now, Ordering::Relaxed);

    let call_count_before = gc.read_cb.consecutive_failures.load(Ordering::Relaxed);
    let result = gc.execute_query("USE theragraph;").await;
    // Should bail early with circuit-open error, not increment failure counter
    assert!(result.is_err());
    // consecutive_failures should NOT change — transport was not called
    assert_eq!(
        gc.read_cb.consecutive_failures.load(Ordering::Relaxed),
        call_count_before,
        "open circuit should skip transport; consecutive_failures must not increment"
    );
}

// RS-09: parse_nebula_table handles __NULL__ scores without dropping the row.
#[test]
fn parse_nebula_table_handles_null_scores() {
    let output = "\
+--------+---------+\n\
| post_id | score  |\n\
+--------+---------+\n\
| \"nft1\" | 1.5    |\n\
| \"nft2\" | __NULL__ |\n\
| \"nft3\" | 0.8    |\n\
+--------+---------+\n\
";
    let results = parse_nebula_table(output, 1, 2);
    // nft1 and nft3 have numeric scores; nft2 is NULL → score 0.0
    let nft2 = results.iter().find(|(k, _)| k == "nft2");
    assert!(nft2.is_some(), "nft2 (__NULL__ score) should be present");
    assert_eq!(nft2.unwrap().1, 0.0, "NULL score should map to 0.0");
    assert_eq!(results.len(), 3, "all three rows should be parsed");
}
