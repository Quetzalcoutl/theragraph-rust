//! Purchase signals from Elixir Postgres — the recommendation feed's source of
//! truth for "who collected what" (Handoff v10, wayfinder v10-app-migration step 4).
//!
//! The chain-log purchase handlers (`DirectHandlers::handle_purchase`, the Kafka
//! `ContentCopyMinted` arm) never produced a signal: they read camelCase keys
//! from snake_case payloads, and they decode only TheraFriendz v1 topics from
//! the v1 address. Elixir already projects every purchase — v1 and v2 (fast
//! lane `CopyPurchased`) — into `purchases`, so this follower tails that table
//! instead of re-decoding the chain.
//!
//! - Cursor: `(inserted_at, id)` in the rec DB (`purchase_signal_cursor`), so a
//!   restart resumes exactly where it stopped; it advances per page, after the
//!   whole page is recorded.
//! - Idempotent: `user_interactions` is unique on (user, nft, type) for
//!   non-view types, so a purchase already recorded (e.g. via
//!   POST /api/v1/interactions) is a no-op, and preferences only move on insert.
//! - First run starts `backfill_hours` back instead of replaying all history.

use chrono::{Duration as ChronoDuration, NaiveDateTime, Utc};
use sqlx::PgPool;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::kafka::BlockchainEvent;
use crate::recommendation::cache::RecCache;
use crate::recommendation::preferences::InteractionType;

use super::interaction::enrich_and_record_pools;

/// Rows per poll — one page is recorded before the cursor moves.
pub const PAGE_SIZE: i64 = 500;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct PurchaseRow {
    pub id: Uuid,
    pub inserted_at: NaiveDateTime,
    pub buyer_address: String,
    pub tx_hash: String,
    pub block_number: i64,
    pub log_index: Option<i32>,
    /// The ORIGINAL's row — purchases.nft_id always points at the original.
    pub nft_uuid: Uuid,
    pub contract_address: String,
    pub contract_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub inserted_at: NaiveDateTime,
    pub id: Uuid,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PollStats {
    pub fetched: usize,
    pub recorded: usize,
    pub failed: usize,
}

/// Synthetic event for `enrich_and_record_pools`: its `event_id` is
/// `"{tx}:{event_type}"`, so the event type carries the log index (v2) or the
/// purchase id (v1 — one purchase per tx there, but ids keep it unique anyway).
pub(crate) fn event_for(row: &PurchaseRow) -> BlockchainEvent {
    let discriminator = match row.log_index {
        Some(i) => format!("Purchase:{i}"),
        None => format!("Purchase:{}", row.id),
    };
    BlockchainEvent {
        event_type: discriminator,
        contract_address: row.contract_address.to_lowercase(),
        contract_type: row.contract_type.clone().unwrap_or_default(),
        block_number: row.block_number.max(0) as u64,
        transaction_hash: row.tx_hash.clone(),
        log_index: row.log_index.unwrap_or(0).max(0) as u64,
        timestamp: row.inserted_at.and_utc().timestamp(),
        data: None,
    }
}

/// Where a fresh follower starts: `backfill_hours` ago, nil id.
pub fn initial_cursor(backfill_hours: u64) -> Cursor {
    Cursor {
        inserted_at: (Utc::now() - ChronoDuration::hours(backfill_hours as i64)).naive_utc(),
        id: Uuid::nil(),
    }
}

pub async fn load_cursor(rec_pool: &PgPool) -> anyhow::Result<Option<Cursor>> {
    let row: Option<(NaiveDateTime, Uuid)> = sqlx::query_as(
        "SELECT last_inserted_at, last_purchase_id FROM purchase_signal_cursor WHERE id = 1",
    )
    .fetch_optional(rec_pool)
    .await?;
    Ok(row.map(|(inserted_at, id)| Cursor { inserted_at, id }))
}

pub async fn save_cursor(rec_pool: &PgPool, c: Cursor) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO purchase_signal_cursor (id, last_inserted_at, last_purchase_id, updated_at) \
         VALUES (1, $1, $2, NOW()) \
         ON CONFLICT (id) DO UPDATE SET last_inserted_at = EXCLUDED.last_inserted_at, \
           last_purchase_id = EXCLUDED.last_purchase_id, updated_at = NOW()",
    )
    .bind(c.inserted_at)
    .bind(c.id)
    .execute(rec_pool)
    .await?;
    Ok(())
}

/// `purchases.log_index` arrives with Elixir migration 20260926000001 (v2 fast
/// lane). Until that migration has run, read it as NULL instead of failing.
async fn has_log_index(elixir_pool: &PgPool) -> anyhow::Result<bool> {
    let (exists,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = 'purchases' AND column_name = 'log_index')",
    )
    .fetch_one(elixir_pool)
    .await?;
    Ok(exists)
}

macro_rules! page_sql {
    ($log_index:literal) => {
        concat!(
            "SELECT p.id, p.inserted_at, p.buyer_address, p.tx_hash, p.block_number::bigint AS block_number, ",
            $log_index,
            ", n.id AS nft_uuid, n.contract_address, n.contract_type::text AS contract_type \
             FROM purchases p \
             JOIN nfts n ON n.id = p.nft_id \
             WHERE (p.inserted_at, p.id) > ($1, $2) \
             ORDER BY p.inserted_at, p.id \
             LIMIT $3"
        )
    };
}
const PAGE_SQL: &str = page_sql!("p.log_index");
const PAGE_SQL_PRE_V2: &str = page_sql!("NULL::int AS log_index");

pub(crate) async fn fetch_page(elixir_pool: &PgPool, after: Cursor) -> anyhow::Result<Vec<PurchaseRow>> {
    let sql = if has_log_index(elixir_pool).await? { PAGE_SQL } else { PAGE_SQL_PRE_V2 };
    let rows = sqlx::query_as::<_, PurchaseRow>(sql)
        .bind(after.inserted_at)
        .bind(after.id)
        .bind(PAGE_SIZE)
        .fetch_all(elixir_pool)
        .await?;
    Ok(rows)
}

/// One poll: record every purchase after the stored cursor, one page at a time,
/// until caught up. A failed row is logged and skipped (the Nebula reconciler
/// still replays its graph edge); a failed page fetch stops the poll without
/// moving the cursor.
pub async fn poll_once(
    rec_pool: &PgPool,
    elixir_pool: &PgPool,
    cache: Option<&RecCache>,
    backfill_hours: u64,
) -> anyhow::Result<PollStats> {
    let mut cursor = match load_cursor(rec_pool).await? {
        Some(c) => c,
        None => initial_cursor(backfill_hours),
    };
    let mut stats = PollStats::default();

    loop {
        let page = fetch_page(elixir_pool, cursor).await?;
        if page.is_empty() {
            break;
        }
        stats.fetched += page.len();
        for row in &page {
            let buyer = row.buyer_address.to_lowercase();
            if buyer.is_empty() {
                continue;
            }
            let event = event_for(row);
            match enrich_and_record_pools(
                rec_pool,
                elixir_pool,
                cache,
                &row.nft_uuid,
                None,
                &buyer,
                InteractionType::Purchase,
                "marketplace",
                &event.contract_type,
                &event,
            )
            .await
            {
                Ok(()) => stats.recorded += 1,
                Err(e) => {
                    stats.failed += 1;
                    warn!(purchase = %row.id, "purchase signal failed: {e}");
                }
            }
        }
        let last = page.last().expect("non-empty page");
        cursor = Cursor { inserted_at: last.inserted_at, id: last.id };
        save_cursor(rec_pool, cursor).await?;
        if (page.len() as i64) < PAGE_SIZE {
            break;
        }
    }

    if stats.fetched > 0 {
        debug!(?stats, "purchase signals: poll complete");
    }
    Ok(stats)
}

#[cfg(test)]
mod tests;
