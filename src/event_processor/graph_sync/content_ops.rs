//! Content lifecycle operations: mint listing price, listing updates.
//!
//! Lazy-mint economy (2026-09-13): `ContentMinted`'s `price` is no longer
//! always 0, and the new `ListingUpdated` event carries the creator's
//! price/max_copies for a voucher-based listing. Distinct from `social_ops`
//! (signals BETWEEN a user and a post: follow/like/comment/purchase) and
//! `media_ops` (consumption signals) — these operations write directly onto
//! the `post` vertex itself, persisting the creator's own listing terms so a
//! future price-aware scoring pass (or a direct graph read) has somewhere to
//! find live price/supply data. See migration 26
//! (`theragraph-nebula/init/26-add-nft-listing-fields.ngql`).

use crate::recommendation::graph_client::GraphTransport;
use crate::recommendation::schema_consts::{
    SPACE_THERAGRAPH, PROP_PRICE, PROP_MAX_COPIES,
    vid_post, is_safe_post_vid_id,
    ensure_post_vertex_nql,
};
use tracing::{info, instrument};

use super::{GraphSync, GraphSyncError};

impl<T: GraphTransport> GraphSync<T> {
    /// Set the real listing `price` on a freshly minted content's `post` vertex.
    ///
    /// `token_id` is the Postgres UUID (`nfts.id`) of the original NFT, not the
    /// raw on-chain token id — same VID convention every other `graph_sync`
    /// write uses, so this lands on the vertex likes/comments/purchases
    /// already write to instead of a disconnected `"post:<int>"` one.
    ///
    /// Upserts the vertex first (mirrors every other `graph_sync` write) since
    /// a mint event may reach Nebula before any social interaction has.
    /// Transient Nebula failures are retried up to 3 times with exponential
    /// backoff.
    #[instrument(skip(self), fields(op = "sync_content_minted", token_id = %token_id))]
    pub async fn sync_content_minted(
        &self,
        token_id: &str,
        price: f64,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_content_minted";

        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("token_id={token_id}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_content_minted(token_id, price))
            .await
    }

    async fn inner_sync_content_minted(
        &self,
        token_id: &str,
        price: f64,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_content_minted";
        let post_vid = vid_post(token_id);
        let query = format!(
            "USE {space};\n{upsert_post}\nUPDATE VERTEX ON post \"{post_vid}\" SET {p_price} = {price};",
            space = SPACE_THERAGRAPH,
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            p_price = PROP_PRICE,
            post_vid = post_vid,
            price = price,
        );

        self.run_write(OP, &query).await?;
        info!("GraphSync: content_minted price written post={} price={}", token_id, price);
        Ok(())
    }

    /// Update `price`/`max_copies` on an existing content's `post` vertex.
    ///
    /// Called for `ListingUpdated` — fires on a voucher's first redemption and
    /// on every later `updateListing` call. Same VID convention as
    /// `sync_content_minted`: `token_id` is the Postgres UUID, not the raw
    /// on-chain token id.
    ///
    /// Upserts the vertex first: `ensure_post_vertex_nql`'s `INSERT ... IF NOT
    /// EXISTS` makes this handler robust to any out-of-order Kafka delivery,
    /// even though in practice `ContentMinted` for the same token has always
    /// already been processed (they fire in the same `redeemVoucher` tx).
    /// Transient Nebula failures are retried up to 3 times with exponential
    /// backoff.
    #[instrument(skip(self), fields(op = "sync_listing_updated", token_id = %token_id))]
    pub async fn sync_listing_updated(
        &self,
        token_id: &str,
        price: f64,
        max_copies: i64,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_listing_updated";

        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("token_id={token_id}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_listing_updated(token_id, price, max_copies))
            .await
    }

    async fn inner_sync_listing_updated(
        &self,
        token_id: &str,
        price: f64,
        max_copies: i64,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_listing_updated";
        let post_vid = vid_post(token_id);
        let query = format!(
            "USE {space};\n{upsert_post}\nUPDATE VERTEX ON post \"{post_vid}\" SET {p_price} = {price}, {p_max_copies} = {max_copies};",
            space = SPACE_THERAGRAPH,
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            p_price = PROP_PRICE,
            p_max_copies = PROP_MAX_COPIES,
            post_vid = post_vid,
            price = price,
            max_copies = max_copies,
        );

        self.run_write(OP, &query).await?;
        info!(
            "GraphSync: listing_updated post={} price={} max_copies={}",
            token_id, price, max_copies
        );
        Ok(())
    }
}
