// ElixirDb adapter — the single place that knows the Elixir DB schema.
// All cross-DB SQL for the event processor lives here; callers receive domain types.

use crate::error::{Error, Result};
use crate::recommendation::cache::RecCache;
use crate::recommendation::features::{extract_features, save_features};
use crate::recommendation::schema_consts::sanitize_genre_slug;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use super::{EnrichmentTask, EventDispatcher};

/// Normalize an NFT's `genre`/`genres` columns (free-text, creator-supplied
/// hashtags) into deduplicated kebab-case slugs — the canonical tag
/// identifier shared by `tag_preferences` (learning, via `nft_metadata`/
/// `lookup_nft_with_metadata`) and `nft_features.tags` (candidate-retrieval
/// matching, via `process_enrichment`). Both sides of the `&&` array-overlap
/// match in `candidate_repository` must agree on the same string form, or a
/// genre-derived tag silently never matches despite representing the same
/// genre — normalize once, here, for every caller.
fn genre_slugs(genre: Option<&str>, genres: &[String]) -> Vec<String> {
    let mut slugs: Vec<String> = Vec::new();
    for raw in genre.into_iter().chain(genres.iter().map(String::as_str)) {
        if let Some(slug) = sanitize_genre_slug(raw) {
            if !slugs.contains(&slug) {
                slugs.push(slug);
            }
        }
    }
    slugs
}

impl EventDispatcher {
    /// Return the actual UUID for a contract/token pair, or None when not yet indexed.
    #[allow(dead_code)]
    pub(super) async fn lookup_nft_uuid(
        &self,
        contract_address: &str,
        token_id: &str,
    ) -> Result<Option<Uuid>> {
        // POOL-002: u256 token IDs (ERC-1155, large NFT series) don't fit i64.
        // Return Ok(None) so the caller treats the NFT as "not yet indexed" and
        // falls through to the enrichment path, rather than propagating a permanent
        // Error::InvalidFormat that commits the Kafka offset and loses the event.
        let token_id_int: i64 = match token_id.parse() {
            Ok(n) => n,
            Err(_) => {
                warn!("lookup_nft_uuid: token_id overflows i64, treating as not-found: {token_id:?}");
                return Ok(None);
            }
        };

        let result: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM nfts WHERE contract_address = $1 AND token_id = $2 LIMIT 1",
        )
        .bind(contract_address.to_lowercase())
        .bind(token_id_int)
        .fetch_optional(&self.elixir_pool)
        .await
        .map_err(|e| Error::Database {
            message: "Failed to lookup NFT".into(),
            source: Some(e),
        })?;

        Ok(result.map(|(id,)| id))
    }

    /// Deterministic v5 UUID from contract address + token ID.
    /// Kept for legacy-event handlers that have no DB lookup path.
    #[allow(dead_code)]
    pub(super) fn generate_nft_uuid(contract_address: &str, token_id: &str) -> Uuid {

        let combined = format!("{}:{}", contract_address.to_lowercase(), token_id);
        Uuid::new_v5(&Uuid::NAMESPACE_OID, combined.as_bytes())
    }
}

/// POOL-005: Single combined query — contract/token lookup + metadata in one round-trip.
///
/// Handlers that previously called `lookup_nft_uuid` then `nft_metadata` can use this
/// instead to save one DB round-trip per event.
#[derive(sqlx::FromRow)]
struct NftWithMetadataRow {
    id: Uuid,
    contract_type: String,
    creator_address: String,
    genre: Option<String>,
    genres: Vec<String>,
}

pub(crate) async fn lookup_nft_with_metadata(
    elixir_pool: &PgPool,
    contract_address: &str,
    token_id_str: &str,
) -> crate::error::Result<Option<(Uuid, super::NftMetadata)>> {
    use crate::error::Error;
    let token_id_int: i64 = match token_id_str.parse() {
        Ok(n) => n,
        Err(_) => return Ok(None),
    };
    // BUG-TAGS-01: this used to select a `tags` column that has never existed
    // on `nfts` (only `genre`/`genres` do — confirmed against the live schema)
    // — every call errored, silently swallowed by callers into an empty-tags
    // fallback. `genre`/`genres` are the real, populated columns.
    let result = sqlx::query_as::<_, NftWithMetadataRow>(
        "SELECT id, contract_type::text, creator_address, genre, \
         COALESCE(genres, ARRAY[]::text[]) AS genres \
         FROM nfts WHERE contract_address = $1 AND token_id = $2 LIMIT 1",
    )
    .bind(contract_address.to_lowercase())
    .bind(token_id_int)
    .fetch_optional(elixir_pool)
    .await
    .map_err(|e| Error::Database {
        message: "lookup_nft_with_metadata failed".into(),
        source: Some(e),
    })?;

    Ok(result.map(|r| {
        let tags = genre_slugs(r.genre.as_deref(), &r.genres);
        let meta = super::NftMetadata {
            contract_type: r.contract_type,
            creator_address: r.creator_address,
            tags,
        };
        (r.id, meta)
    }))
}

/// Fetch creator, contract_type, and tags for an NFT UUID.
/// Single entry point — used by DirectHandlers and any caller with only a &PgPool.
#[derive(sqlx::FromRow)]
struct NftMetadataQueryRow {
    contract_type: String,
    creator_address: String,
    genre: Option<String>,
    genres: Vec<String>,
}

pub(crate) async fn nft_metadata(
    elixir_pool: &PgPool,
    nft_id: &Uuid,
) -> crate::error::Result<Option<super::NftMetadata>> {
    use crate::error::Error;
    // BUG-TAGS-01: see lookup_nft_with_metadata above — `tags` never existed
    // on `nfts`; this query errored on every call, and the caller's fallback
    // silently supplied empty tags for every interaction. Read the real
    // `genre`/`genres` columns instead.
    let result = sqlx::query_as::<_, NftMetadataQueryRow>(
        "SELECT contract_type::text, creator_address, genre, \
         COALESCE(genres, ARRAY[]::text[]) AS genres \
         FROM nfts WHERE id = $1",
    )
    .bind(nft_id)
    .fetch_optional(elixir_pool)
    .await
    .map_err(|e| Error::Database {
        message: "lookup_nft_metadata failed".into(),
        source: Some(e),
    })?;

    Ok(result.map(|r| super::NftMetadata {
        contract_type: r.contract_type,
        tags: genre_slugs(r.genre.as_deref(), &r.genres),
        creator_address: r.creator_address,
    }))
}

/// Pull real genres from Elixir `nfts` and write to rec-DB `nft_features`.
/// Called by the enrichment worker — never blocks the Kafka consumer loop.
/// Race condition (NFT not yet visible in Elixir) is non-fatal; score updater retries.
///
/// GENRE-01 / BUG-TAGS-01: this previously also selected a `tags` column that
/// never existed on `nfts` (the query errored on every call — see
/// `genre_slugs` doc comment above) and fed that phantom, always-empty value
/// into `extract_features` as a second, redundant "legacy tags" source
/// alongside the genre slugs computed below. `genre`/`genres` were always the
/// only real signal here; this now folds them into `nft_features.tags` as
/// kebab-case slugs — the crate's canonical genre identifier, shared with the
/// `genre_preference` Nebula edges (see `graph_client::edges::
/// write_genre_preference_edges`) and the frontend's genre taxonomy — with no
/// separate genre-specific scoring code needed.
pub(super) async fn process_enrichment(
    task: EnrichmentTask,
    pool: &PgPool,
    elixir_pool: &PgPool,
    cache: Option<&RecCache>,
) {
    #[derive(sqlx::FromRow)]
    struct NftRow {
        contract_type: String,
        creator_address: String,
        genre: Option<String>,
        genres: Option<Vec<String>>,
    }

    let row = sqlx::query_as::<_, NftRow>(
        "SELECT contract_type::text, creator_address, genre, \
         COALESCE(genres, '{}') AS genres \
         FROM nfts WHERE id = $1",
    )
    .bind(task.nft_uuid)
    .fetch_optional(elixir_pool)
    .await;

    let (contract_type, _creator_address, genre, genres) = match row {
        Ok(Some(r)) => (
            r.contract_type,
            r.creator_address,
            r.genre,
            r.genres.unwrap_or_default(),
        ),
        Ok(None) => return, // race — NFT not yet indexed; a future interaction re-enqueues
        Err(e) => {
            warn!("process_enrichment: Elixir DB query failed: {}", e);
            return;
        }
    };

    let slugs = genre_slugs(genre.as_deref(), &genres);

    let metadata = serde_json::json!({});
    let mut features = extract_features(
        &task.nft_uuid.to_string(),
        &task.contract_address,
        task.token_id,
        &contract_type,
        &metadata,
        0.5,
    );
    const MAX_TAGS: usize = 10;
    for tag in &slugs {
        if features.tags.len() >= MAX_TAGS { break; }
        if !features.tags.contains(tag) {
            features.tags.push(tag.clone());
        }
    }
    features.tags.sort();

    if let Err(e) = save_features(pool, &features).await {
        warn!("process_enrichment: save_features failed: {}", e);
    } else if let Some(cache) = cache {
        cache.delete_nft_features(&task.nft_uuid.to_string()).await;
    }
}

/// Counts from one `backfill_historical_nft_tags` pass.
pub(crate) struct BackfillStats {
    pub(crate) nfts_considered: u64,
    pub(crate) nfts_with_genres: u64,
    pub(crate) rows_updated: u64,
}

/// Chunk size for `backfill_historical_nft_tags`'s batched read/write —
/// mirrors `event_processor::reconciliation::RECONCILE_PAGE_SIZE`'s reasoning:
/// bounds memory/query size per round trip regardless of how many NFTs need
/// backfilling, at a small, safe cost of extra passes for very large runs.
const BACKFILL_CHUNK_SIZE: usize = 5_000;

#[derive(sqlx::FromRow)]
struct BackfillNftRow {
    id: Uuid,
    genre: Option<String>,
    genres: Vec<String>,
}

/// One-time backfill: for `user_interactions` rows whose `nft_tags` is
/// empty — the historical damage from BUG-TAGS-01 (the `nfts.tags` column
/// that never existed, fixed above in `nft_metadata`/`lookup_nft_with_metadata`/
/// `process_enrichment`) — look up each referenced NFT's *current*
/// genre/genres and fill `nft_tags` in with the same normalized slugs the
/// live path now produces. Best-effort: an NFT's genres may have changed
/// since the interaction happened, but this recovers real signal instead of
/// leaving it permanently empty.
///
/// Batched, not N+1: processes `BACKFILL_CHUNK_SIZE` NFTs per round trip —
/// one `WHERE id = ANY($1)` read and one JSONB-driven bulk `UPDATE` per
/// chunk, rather than one query per NFT.
///
/// Exposed at `POST /admin/backfill-nft-tags` (internal-key gated —
/// `api::profile::backfill_nft_tags`). Run
/// `recommendation::preferences::rebuild_all_user_preferences` afterward so
/// `tag_preferences`/`creator_preferences` get regenerated from the
/// newly-populated history.
pub(crate) async fn backfill_historical_nft_tags(
    rec_pool: &PgPool,
    elixir_pool: &PgPool,
) -> Result<BackfillStats> {
    let empty_nft_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT nft_id FROM user_interactions WHERE cardinality(nft_tags) = 0",
    )
    .fetch_all(rec_pool)
    .await
    .map_err(|e| Error::Database {
        message: "backfill_historical_nft_tags: failed to list empty-tag nft_ids".into(),
        source: Some(e),
    })?;

    let mut stats = BackfillStats {
        nfts_considered: empty_nft_ids.len() as u64,
        nfts_with_genres: 0,
        rows_updated: 0,
    };

    for chunk in empty_nft_ids.chunks(BACKFILL_CHUNK_SIZE) {
        // One batched read for the whole chunk instead of one query per NFT.
        let nft_rows: Vec<BackfillNftRow> = sqlx::query_as(
            "SELECT id, genre, COALESCE(genres, ARRAY[]::text[]) AS genres \
             FROM nfts WHERE id = ANY($1)",
        )
        .bind(chunk)
        .fetch_all(elixir_pool)
        .await
        .map_err(|e| Error::Database {
            message: "backfill_historical_nft_tags: batched genre lookup failed".into(),
            source: Some(e),
        })?;

        // Build one JSON payload {id, tags}[] for every NFT in the chunk
        // that actually has genre data — NFTs with no genres, or that no
        // longer exist (burned), are simply absent from nft_rows/this list
        // and left untouched.
        let mut payload: Vec<serde_json::Value> = Vec::with_capacity(nft_rows.len());
        for row in nft_rows {
            let slugs = genre_slugs(row.genre.as_deref(), &row.genres);
            if slugs.is_empty() {
                continue;
            }
            stats.nfts_with_genres += 1;
            payload.push(serde_json::json!({ "id": row.id.to_string(), "tags": slugs }));
        }

        if payload.is_empty() {
            continue;
        }

        // One batched UPDATE for the whole chunk instead of one per NFT.
        let result = sqlx::query(
            "UPDATE user_interactions ui \
             SET nft_tags = ARRAY(SELECT jsonb_array_elements_text(elem->'tags')) \
             FROM jsonb_array_elements($1::jsonb) AS elem \
             WHERE ui.nft_id = (elem->>'id')::uuid \
               AND cardinality(ui.nft_tags) = 0",
        )
        .bind(serde_json::Value::Array(payload))
        .execute(rec_pool)
        .await
        .map_err(|e| Error::Database {
            message: "backfill_historical_nft_tags: batched update failed".into(),
            source: Some(e),
        })?;
        stats.rows_updated += result.rows_affected();
    }

    info!(
        nfts_considered = stats.nfts_considered,
        nfts_with_genres = stats.nfts_with_genres,
        rows_updated = stats.rows_updated,
        "backfill_historical_nft_tags: pass complete"
    );

    Ok(stats)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod backfill_live {
    //! One-time, hand-run recovery script — not part of the normal test
    //! suite's signal. `cargo test` never runs this (ignored by default);
    //! run it explicitly and once with:
    //!   cargo test --lib event_processor::elixir_db::backfill_live -- --ignored --nocapture
    //! (also reachable at runtime via POST /admin/backfill-nft-tags —
    //! this test is a CLI-only alternative that doesn't need the server up)
    use super::*;

    #[tokio::test]
    #[ignore = "one-time historical data recovery — run manually against a real DB, not part of CI"]
    async fn run_backfill_historical_nft_tags() {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let elixir_database_url =
            std::env::var("ELIXIR_DATABASE_URL").unwrap_or_else(|_| database_url.clone());

        let rec_pool = PgPool::connect(&database_url).await.expect("connect rec_pool");
        let elixir_pool = if elixir_database_url == database_url {
            rec_pool.clone()
        } else {
            PgPool::connect(&elixir_database_url).await.expect("connect elixir_pool")
        };

        let stats = backfill_historical_nft_tags(&rec_pool, &elixir_pool)
            .await
            .expect("backfill_historical_nft_tags failed");

        println!(
            "backfill complete: {} NFTs considered, {} had genres, {} user_interactions rows updated",
            stats.nfts_considered, stats.nfts_with_genres, stats.rows_updated
        );
    }

    /// The run above only proves the batched query *runs* — on this dev DB
    /// there's nothing to backfill, so it never actually exercises the
    /// batched JSONB UPDATE against a real empty-tag row. This test seeds
    /// one, under a throwaway NFT + interaction, and confirms the row is
    /// actually filled in correctly before cleaning up.
    #[tokio::test]
    #[ignore = "hits a real database — run manually, not part of CI"]
    async fn backfill_actually_fills_a_real_empty_tag_row() {
        const TEST_CONTRACT: &str = "0xbackfilllivetest0000000000000000000001";
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let pool = PgPool::connect(&database_url).await.expect("connect pool");

        // Throwaway NFT with real genre data — nfts.id/token_id/etc. carry
        // several NOT NULL columns with no defaults, so this is the minimal
        // valid row.
        let token_id = (Uuid::new_v4().as_u128() & 0x7FFF_FFFF_FFFF_FFFF) as i64;
        let (nft_id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO nfts \
             (id, token_id, contract_address, contract_type, creator_address, owner_address, \
              created_at_block, inserted_at, updated_at, genre, genres) \
             VALUES (gen_random_uuid(), $1, $2, 'art', '0xcreatorbackfilltest', '0xcreatorbackfilltest', \
                     0, NOW(), NOW(), 'Minimalism', ARRAY['minimalism']) \
             RETURNING id",
        )
        .bind(token_id)
        .bind(TEST_CONTRACT)
        .fetch_one(&pool)
        .await
        .expect("seed nft failed");

        // An interaction referencing it, with the empty nft_tags BUG-TAGS-01
        // left behind.
        sqlx::query(
            "INSERT INTO user_interactions \
             (id, event_id, user_address, nft_id, interaction_type, nft_contract_type, \
              nft_creator_address, nft_tags, created_at) \
             VALUES (gen_random_uuid(), $1, '0xbackfilllivetestuser000000000000000001', \
                     $2, 'like', 'art', '0xcreatorbackfilltest', '{}', NOW())",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(nft_id)
        .execute(&pool)
        .await
        .expect("seed interaction failed");

        let stats = backfill_historical_nft_tags(&pool, &pool)
            .await
            .expect("backfill_historical_nft_tags failed");

        assert!(stats.nfts_considered >= 1);
        assert!(stats.nfts_with_genres >= 1);
        assert!(stats.rows_updated >= 1);

        let (nft_tags,): (Vec<String>,) =
            sqlx::query_as("SELECT nft_tags FROM user_interactions WHERE nft_id = $1")
                .bind(nft_id)
                .fetch_one(&pool)
                .await
                .expect("expected the seeded row to still exist");
        assert_eq!(nft_tags, vec!["minimalism".to_string()]);

        // Cleanup.
        sqlx::query("DELETE FROM user_interactions WHERE nft_id = $1")
            .bind(nft_id)
            .execute(&pool)
            .await
            .expect("cleanup user_interactions failed");
        sqlx::query("DELETE FROM nfts WHERE contract_address = $1")
            .bind(TEST_CONTRACT)
            .execute(&pool)
            .await
            .expect("cleanup test nft failed");
    }
}
