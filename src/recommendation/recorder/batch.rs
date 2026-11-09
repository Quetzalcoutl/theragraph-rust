//! Batched VIEW recording: one request for the views a person accrued in a short client window.
//!
//! Per view the single path (`record_interaction`) costs an INSERT, a preferences read-modify-write transaction (4
//! statements with BEGIN and COMMIT), a cache purge (two Redis DELs and a SQL DELETE) and a session-signal pipeline.
//! A batch of N views costs: ONE multi-row INSERT, ONE preferences transaction for all the inserted views, ONE dirty
//! mark (or one purge in the legacy mode) and ONE session-signal pipeline. Only views are batched: a like, a follow or
//! any purchase must land at once and keeps using the single path.

use std::collections::HashSet;

use anyhow::{bail, Result};
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use super::{load_or_insert_prefs_for_update, purge_user_recommendation_caches, save_preferences, session_signal_of};
use crate::recommendation::cache::{rec_dirty_debounce_secs, RecCache};
use crate::recommendation::model::{
    apply_interaction_to_prefs, interaction_weight, EvictionPolicy, InteractionEvent, InteractionType,
};

/// The most views one batch request carries (the client caps at the same number).
pub const MAX_VIEW_BATCH: usize = 50;

/// Record a batch of view events of ONE user. Returns how many rows were newly recorded.
///
/// Errors if the batch is empty, larger than [`MAX_VIEW_BATCH`], holds anything but views, or mixes users: the HTTP
/// handler filters those out first, this is the belt-and-braces check on the one function that must never batch a like.
pub async fn record_view_batch(pool: &PgPool, events: Vec<InteractionEvent>, cache: Option<&RecCache>) -> Result<usize> {
    if events.is_empty() || events.len() > MAX_VIEW_BATCH {
        bail!("a view batch holds 1 to {MAX_VIEW_BATCH} events, got {}", events.len());
    }
    let user = events[0].user_address.to_lowercase();
    if events.iter().any(|e| e.interaction_type != InteractionType::View || e.user_address.to_lowercase() != user) {
        bail!("a view batch holds only views of one user");
    }

    // 1. ONE multi-row INSERT. `ON CONFLICT DO NOTHING` + RETURNING tells which rows are new (the same dedupe the
    //    single path relies on); each row gets its own idempotency key, like the single path's fresh UUID.
    let mut rows: Vec<(String, Uuid, &InteractionEvent)> = Vec::with_capacity(events.len());
    for ev in &events {
        let Ok(nft) = Uuid::parse_str(&ev.nft_id) else { continue };
        rows.push((ev.event_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string()), nft, ev));
    }
    if rows.is_empty() {
        return Ok(0);
    }
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "INSERT INTO user_interactions \
         (id, event_id, user_address, nft_id, interaction_type, view_duration_ms, source, \
          nft_contract_type, nft_creator_address, nft_tags, pct_played, created_at) ",
    );
    qb.push_values(rows.iter(), |mut b, (event_id, nft, ev)| {
        b.push("gen_random_uuid()")
            .push_bind(event_id.clone())
            .push_bind(user.clone())
            .push_bind(*nft)
            .push_bind(ev.interaction_type.to_string())
            .push_bind(ev.view_duration_ms)
            .push_bind(ev.source.clone())
            .push_bind(ev.nft_contract_type.clone())
            .push_bind(ev.nft_creator_address.clone())
            .push_bind(ev.nft_tags.clone())
            .push_bind(ev.pct_played)
            .push("NOW()");
    });
    qb.push(" ON CONFLICT DO NOTHING RETURNING event_id");
    let inserted_ids: HashSet<String> = qb
        .build_query_as::<(String,)>()
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect();
    let inserted: Vec<&InteractionEvent> = rows
        .iter()
        .filter(|(event_id, _, _)| inserted_ids.contains(event_id))
        .map(|(_, _, ev)| *ev)
        .collect();
    if inserted.is_empty() {
        return Ok(0);
    }

    // 2. ONE preferences transaction for every inserted view (one lock, one read, N applies, one write).
    let mut tx = pool.begin().await?;
    let mut prefs = load_or_insert_prefs_for_update(&mut tx, &user).await?;
    for ev in &inserted {
        apply_interaction_to_prefs(&mut prefs, ev, EvictionPolicy::default());
    }
    save_preferences(&mut *tx, &prefs).await?;
    tx.commit().await?;

    // 3. The cached feed is stale: ONE dirty mark for the whole batch (or one purge in the legacy mode / without
    //    Redis), then ONE pipeline for the session-recency signals.
    if cache.is_some() && rec_dirty_debounce_secs() > 0 {
        if let Some(cache) = cache {
            cache.mark_recs_dirty(&user).await;
        }
    } else {
        purge_user_recommendation_caches(pool, cache, &user).await;
    }
    if let Some(cache) = cache {
        let signals: Vec<_> = inserted
            .iter()
            .filter_map(|ev| {
                let w = interaction_weight(ev);
                (w > 0.0).then(|| session_signal_of(ev, w))
            })
            .collect();
        cache.append_session_signals(&user, signals).await;
    }

    Ok(inserted.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(user: &str, nft: &str) -> InteractionEvent {
        InteractionEvent {
            user_address: user.to_string(),
            nft_id: nft.to_string(),
            interaction_type: InteractionType::View,
            view_duration_ms: Some(4_000),
            pct_played: None,
            source: Some("feed".to_string()),
            nft_contract_type: Some("art".to_string()),
            nft_creator_address: None,
            nft_tags: vec!["moss".to_string()],
            tag_enrichment: Default::default(),
            event_id: None,
        }
    }

    // A pool that never connects: the guards below bail out before any statement is sent.
    fn lazy_pool() -> PgPool {
        PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").expect("a lazy pool builds without connecting")
    }

    #[tokio::test]
    async fn an_empty_or_oversized_batch_is_refused() {
        let pool = lazy_pool();
        assert!(record_view_batch(&pool, vec![], None).await.is_err());
        let many: Vec<_> = (0..MAX_VIEW_BATCH + 1).map(|_| view("0xaaa", &Uuid::new_v4().to_string())).collect();
        assert!(record_view_batch(&pool, many, None).await.is_err());
    }

    #[tokio::test]
    async fn only_views_of_one_user_may_be_batched() {
        let pool = lazy_pool();
        let id = Uuid::new_v4().to_string();
        let mut like = view("0xaaa", &id);
        like.interaction_type = InteractionType::Like;
        assert!(record_view_batch(&pool, vec![view("0xaaa", &id), like], None).await.is_err(), "a like is never batched");
        assert!(
            record_view_batch(&pool, vec![view("0xaaa", &id), view("0xbbb", &id)], None).await.is_err(),
            "two users in one batch"
        );
    }

    // ── Live measurement (runs only with DATABASE_URL, like the other database tests) ──────────────────────────
    //
    // Counts the SQL statements sqlx logs for 20 views recorded one by one (the old path: one request per view)
    // against the same 20 views recorded as ONE batch, and checks both leave identical preference totals.

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, Once};
    use std::thread::ThreadId;
    use tracing_subscriber::layer::SubscriberExt;

    // A process-wide counter that only counts sqlx statements logged on ONE thread (the measuring test's own: a
    // `#[tokio::test]` runtime is single-threaded). A thread-local subscriber proved unreliable when the whole suite
    // runs in parallel (tracing caches callsite interest), and a global one alone would count the other tests' queries.
    static STATEMENTS: AtomicUsize = AtomicUsize::new(0);
    static COUNTED_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);

    struct StatementCounter;

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for StatementCounter {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
            if event.metadata().target() == "sqlx::query"
                && *COUNTED_THREAD.lock().unwrap() == Some(std::thread::current().id())
            {
                STATEMENTS.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// Install the counting subscriber once; false when something else already holds the global slot.
    fn install_counter() -> bool {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(StatementCounter));
        });
        tracing::dispatcher::get_default(|d| d.is::<tracing_subscriber::layer::Layered<StatementCounter, tracing_subscriber::Registry>>())
    }

    async fn make_nfts(pool: &PgPool, creator: &str, n: usize) -> Vec<Uuid> {
        let mut ids = Vec::new();
        for _ in 0..n {
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO nfts (id, contract_address, contract_type, creator_address, owner_address, inserted_at, updated_at) \
                 VALUES ($1, $2, 'art', $3, $3, NOW(), NOW())",
            )
            .bind(id)
            .bind(format!("0x{}", &Uuid::new_v4().simple().to_string()[..32]))
            .bind(creator)
            .execute(pool)
            .await
            .expect("insert test nft");
            ids.push(id);
        }
        ids
    }

    async fn clean(pool: &PgPool, users: &[String], nfts: &[Uuid]) {
        for u in users {
            let _ = sqlx::query("DELETE FROM user_interactions WHERE user_address = $1").bind(u).execute(pool).await;
            let _ = sqlx::query("DELETE FROM user_preferences WHERE user_address = $1").bind(u).execute(pool).await;
            let _ = sqlx::query("DELETE FROM recommendation_cache WHERE user_address = $1").bind(u).execute(pool).await;
        }
        for id in nfts {
            let _ = sqlx::query("DELETE FROM nfts WHERE id = $1").bind(id).execute(pool).await;
        }
    }

    #[tokio::test]
    async fn twenty_views_as_one_batch_cost_far_fewer_statements_than_twenty_single_posts() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else { return };
        let pool = PgPool::connect(&database_url).await.expect("connect");
        let creator = format!("0x{}", &Uuid::new_v4().simple().to_string()[..32]);
        let nfts = make_nfts(&pool, &creator, 20).await;
        // Recognisable addresses so a Redis MONITOR capture can count the commands of each path exactly.
        let single_user = format!("0xmeasuresingle{}", &Uuid::new_v4().simple().to_string()[..16]);
        let batch_user = format!("0xmeasurebatch{}", &Uuid::new_v4().simple().to_string()[..16]);
        let cache = match std::env::var("REDIS_URL") {
            Ok(url) => RecCache::connect(&url).await,
            Err(_) => None,
        };
        let users = vec![single_user.clone(), batch_user.clone()];

        let counting = install_counter();
        *COUNTED_THREAD.lock().unwrap() = Some(std::thread::current().id());
        STATEMENTS.store(0, Ordering::SeqCst);

        // Before: 20 single posts (no Redis here, so each one also purges the SQL cache tier at once).
        for id in &nfts {
            let ev = view(&single_user, &id.to_string());
            super::super::record_interaction(&pool, ev, cache.as_ref()).await.expect("single view");
        }
        let single_statements = STATEMENTS.swap(0, Ordering::SeqCst);

        // After: the same 20 views as one batch.
        let events: Vec<_> = nfts.iter().map(|id| view(&batch_user, &id.to_string())).collect();
        let recorded = record_view_batch(&pool, events, cache.as_ref()).await.expect("batch");
        let batch_statements = STATEMENTS.load(Ordering::SeqCst);

        let rows_single: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_interactions WHERE user_address = $1")
            .bind(&single_user).fetch_one(&pool).await.unwrap();
        let rows_batch: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_interactions WHERE user_address = $1")
            .bind(&batch_user).fetch_one(&pool).await.unwrap();
        let tags = |u: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, serde_json::Value>("SELECT tag_preferences FROM user_preferences WHERE user_address = $1")
                    .bind(u).fetch_one(&pool).await.unwrap()
            }
        };
        let prefs_single = tags(single_user.clone()).await;
        let prefs_batch = tags(batch_user.clone()).await;
        clean(&pool, &users, &nfts).await;

        eprintln!("MEASURED (redis: {}, debounce {} s) 20 views: single path {single_statements} SQL statements ({} per view), batch {batch_statements} ({} per view)",
            cache.is_some(), rec_dirty_debounce_secs(), single_statements as f64 / 20.0, batch_statements as f64 / 20.0);
        assert_eq!(recorded, 20);
        assert_eq!(rows_single, 20);
        assert_eq!(rows_batch, 20);
        assert_eq!(prefs_single, prefs_batch, "applying the views as a batch leaves the same tag preferences");
        if counting {
            assert!(single_statements > 0, "the counter saw no statements at all");
            assert!(batch_statements * 4 < single_statements, "{batch_statements} batch vs {single_statements} single");
        } else {
            eprintln!("statement counting unavailable (another global subscriber is installed): equivalence checks only");
        }
    }
}
