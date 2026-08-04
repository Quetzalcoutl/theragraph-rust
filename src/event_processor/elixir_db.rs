// ElixirDb adapter — the single place that knows the Elixir DB schema.
// All cross-DB SQL for the event processor lives here; callers receive domain types.

use crate::error::{Error, Result};
use crate::recommendation::cache::RecCache;
use crate::recommendation::features::{extract_features, save_features};
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use super::{EnrichmentTask, EventProcessor};

impl EventProcessor {
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
    tags: Vec<String>,
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
    let result = sqlx::query_as::<_, NftWithMetadataRow>(
        "SELECT id, contract_type::text, creator_address, \
         COALESCE(tags, ARRAY[]::text[]) AS tags \
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
        let meta = super::NftMetadata {
            contract_type: r.contract_type,
            creator_address: r.creator_address,
            tags: r.tags,
        };
        (r.id, meta)
    }))
}

/// Fetch creator, contract_type, and tags for an NFT UUID.
/// Single entry point — used by DirectHandlers and any caller with only a &PgPool.
pub(crate) async fn nft_metadata(
    elixir_pool: &PgPool,
    nft_id: &Uuid,
) -> crate::error::Result<Option<super::NftMetadata>> {
    use crate::error::Error;
    let result = sqlx::query_as::<_, super::NftMetadataRow>(
        "SELECT contract_type::text, creator_address, \
         COALESCE(tags, ARRAY[]::text[]) AS tags \
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
        creator_address: r.creator_address,
        tags: r.tags,
    }))
}

/// Pull real tags from Elixir `nfts` and write to rec-DB `nft_features`.
/// Called by the enrichment worker — never blocks the Kafka consumer loop.
/// Race condition (NFT not yet visible in Elixir) is non-fatal; score updater retries.
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
        tags: Option<Vec<String>>,
    }

    let row = sqlx::query_as::<_, NftRow>(
        "SELECT contract_type::text, creator_address, \
         COALESCE(tags, '{}') AS tags FROM nfts WHERE id = $1",
    )
    .bind(task.nft_uuid)
    .fetch_optional(elixir_pool)
    .await;

    let (contract_type, _creator_address, tags) = match row {
        Ok(Some(r)) => (r.contract_type, r.creator_address, r.tags.unwrap_or_default()),
        Ok(None) => return, // race — NFT not yet indexed; a future interaction re-enqueues
        Err(e) => {
            warn!("process_enrichment: Elixir DB query failed: {}", e);
            return;
        }
    };

    let metadata = serde_json::json!({ "tags": tags });
    let mut features = extract_features(
        &task.nft_uuid.to_string(),
        &task.contract_address,
        task.token_id,
        &contract_type,
        &metadata,
        0.5,
    );
    const MAX_TAGS: usize = 10;
    for tag in &tags {
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

#[cfg(test)]
mod tests {
    use super::super::EventProcessor;
    use uuid::Version;

    #[test]
    fn generate_nft_uuid_is_deterministic() {
        let a = EventProcessor::generate_nft_uuid("0xABCDEF", "42");
        let b = EventProcessor::generate_nft_uuid("0xABCDEF", "42");
        assert_eq!(a, b);
    }

    #[test]
    fn generate_nft_uuid_lowercases_contract_address() {
        let upper = EventProcessor::generate_nft_uuid("0xABCDEF", "1");
        let lower = EventProcessor::generate_nft_uuid("0xabcdef", "1");
        assert_eq!(upper, lower);
    }

    #[test]
    fn generate_nft_uuid_different_contract_yields_different_uuid() {
        let a = EventProcessor::generate_nft_uuid("0xAAAA", "1");
        let b = EventProcessor::generate_nft_uuid("0xBBBB", "1");
        assert_ne!(a, b);
    }

    #[test]
    fn generate_nft_uuid_different_token_id_yields_different_uuid() {
        let a = EventProcessor::generate_nft_uuid("0xAAAA", "1");
        let b = EventProcessor::generate_nft_uuid("0xAAAA", "2");
        assert_ne!(a, b);
    }

    #[test]
    fn generate_nft_uuid_output_is_version_5() {
        let uuid = EventProcessor::generate_nft_uuid("0xABCDEF", "99");
        assert_eq!(uuid.get_version(), Some(Version::Sha1));
    }
}
