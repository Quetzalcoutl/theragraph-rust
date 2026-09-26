//! Content engagement operations: bookmark, unbookmark, share.

use crate::recommendation::graph_client::{GraphTransport, normalize_address};
use crate::recommendation::schema_consts::{
    SPACE_THERAGRAPH, EDGE_BOOKMARKED, EDGE_SHARED,
    PROP_EVENT_ID, PROP_BOOKMARKED_AT, PROP_SHARED_AT, PROP_WEIGHT,
    vid_user, vid_post,
    is_safe_address, is_safe_id, is_safe_post_vid_id,
    ensure_user_vertex_nql, ensure_post_vertex_nql,
};
use tracing::{info, instrument};

use super::{GraphSync, GraphSyncError};

impl<T: GraphTransport> GraphSync<T> {
    /// Insert a `bookmarked` edge from user → post.
    ///
    /// Uses `INSERT EDGE IF NOT EXISTS` — bookmarks are a toggle: re-bookmarking
    /// the same NFT should be a no-op. The matching unbookmark path is `sync_unbookmark`.
    #[instrument(skip(self), fields(op = "sync_bookmark", user = %user, token_id = %token_id))]
    pub async fn sync_bookmark(
        &self,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_bookmark";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("token_id={token_id}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_bookmark(token_id, user, tx_hash))
            .await
    }

    async fn inner_sync_bookmark(
        &self,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_bookmark";
        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_bookmarked}({p_eid}, {p_bookmarked_at}) VALUES \"{user_vid}\" -> \"{post_vid}\":(\"{txh}\", now());",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&user_vid, &user),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_bookmarked = EDGE_BOOKMARKED,
            p_eid = PROP_EVENT_ID,
            p_bookmarked_at = PROP_BOOKMARKED_AT,
            user_vid = user_vid,
            post_vid = post_vid,
            txh = tx_hash,
        );

        self.run_write(OP, &query).await?;
        info!("GraphSync: bookmark edge written user={} post={}", user, token_id);
        Ok(())
    }

    /// Delete the `bookmarked` edge on unbookmark.
    #[instrument(skip(self), fields(op = "sync_unbookmark", user = %user, token_id = %token_id))]
    pub async fn sync_unbookmark(
        &self,
        token_id: &str,
        user: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unbookmark";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("token_id={token_id}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_unbookmark(token_id, user))
            .await
    }

    async fn inner_sync_unbookmark(
        &self,
        token_id: &str,
        user: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unbookmark";
        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        let query = format!(
            "USE {space};\nDELETE EDGE {e} \"{src}\" -> \"{dst}\";",
            space = SPACE_THERAGRAPH,
            e = EDGE_BOOKMARKED,
            src = user_vid,
            dst = post_vid,
        );
        self.run_write(OP, &query).await?;
        info!("GraphSync: bookmark edge deleted user={} post={}", user, token_id);
        Ok(())
    }

    /// Insert a `shared` edge from user → post.
    ///
    /// Weight 1.5 — stronger signal than a like (1.0), weaker than a purchase (2.0).
    /// Uses `INSERT EDGE IF NOT EXISTS` so reconciler replays do not overwrite `shared_at`.
    #[instrument(skip(self), fields(op = "sync_share", user = %user, token_id = %token_id))]
    pub async fn sync_share(
        &self,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_share";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("token_id={token_id}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_share(token_id, user, tx_hash))
            .await
    }

    async fn inner_sync_share(
        &self,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_share";
        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        let weight: f64 = 1.5;
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_shared}({p_eid}, {p_shared_at}, {p_weight}) VALUES \"{user_vid}\" -> \"{post_vid}\":(\"{txh}\", now(), {wt});",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&user_vid, &user),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_shared = EDGE_SHARED,
            p_eid = PROP_EVENT_ID,
            p_shared_at = PROP_SHARED_AT,
            p_weight = PROP_WEIGHT,
            user_vid = user_vid,
            post_vid = post_vid,
            txh = tx_hash,
            wt = weight,
        );

        self.run_write(OP, &query).await?;
        info!("GraphSync: share edge written user={} post={}", user, token_id);
        Ok(())
    }
}
