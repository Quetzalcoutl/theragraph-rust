//! Social graph operations: follow, unfollow, like, unlike, comment, purchase.

use crate::recommendation::graph_client::{GraphTransport, map_reaction_weight, normalize_address};
use crate::recommendation::schema_consts::{
    SPACE_THERAGRAPH, EDGE_LIKES, EDGE_PURCHASES, EDGE_COMMENTS_ON, EDGE_FOLLOWS,
    PROP_EVENT_ID, PROP_LIKED_AT, PROP_PURCHASED_AT, PROP_REACTION_TYPE, PROP_WEIGHT,
    PROP_COMMENT_TEXT, PROP_COMMENTED_AT, PROP_FOLLOWED_AT, PROP_COPIES_MINTED,
    vid_user, vid_post, comment_rank,
    is_safe_address, is_safe_id, is_safe_post_vid_id,
    ensure_user_vertex_nql, ensure_post_vertex_nql,
};
use tracing::{info, instrument};

use super::{GraphSync, GraphSyncError};

impl<T: GraphTransport> GraphSync<T> {
    /// Upsert user vertices and insert a `follows` edge.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_follow", follower = %follower, target = %target))]
    pub async fn sync_follow(
        &self,
        follower: &str,
        target: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_follow";

        if !is_safe_address(follower) || !is_safe_address(target) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("follower={follower} target={target}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_follow(follower, target, tx_hash))
            .await
    }

    async fn inner_sync_follow(
        &self,
        follower: &str,
        target: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_follow";

        let follower = normalize_address(follower);
        let target = normalize_address(target);
        let fwr_vid = vid_user(&follower);
        let fwe_vid = vid_user(&target);
        let query = format!(
            "USE {space};\n{upsert_fwr}\n{upsert_fwe}\nINSERT EDGE IF NOT EXISTS {e_follows}({p_eid}, {p_followed_at}, {p_weight}) VALUES \"{fwr_vid}\" -> \"{fwe_vid}\":(\"{eid}\", now(), 1.0);",
            space = SPACE_THERAGRAPH,
            upsert_fwr = ensure_user_vertex_nql(&fwr_vid, &follower),
            upsert_fwe = ensure_user_vertex_nql(&fwe_vid, &target),
            e_follows = EDGE_FOLLOWS,
            p_eid = PROP_EVENT_ID,
            p_followed_at = PROP_FOLLOWED_AT,
            p_weight = PROP_WEIGHT,
            fwr_vid = fwr_vid,
            fwe_vid = fwe_vid,
            eid = tx_hash,
        );

        self.run_write(OP, &query).await?;
        info!("GraphSync: follows edge written {} -> {}", follower, target);
        Ok(())
    }

    /// Delete the `follows` edge on unfollow.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_unfollow", follower = %follower, target = %target))]
    pub async fn sync_unfollow(
        &self,
        follower: &str,
        target: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unfollow";

        if !is_safe_address(follower) || !is_safe_address(target) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("follower={follower} target={target}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_unfollow(follower, target)).await
    }

    async fn inner_sync_unfollow(
        &self,
        follower: &str,
        target: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unfollow";
        let follower = normalize_address(follower);
        let target   = normalize_address(target);
        let fwr_vid  = vid_user(&follower);
        let fwe_vid  = vid_user(&target);
        let query = format!(
            "USE {space};\nDELETE EDGE {e} \"{src}\" -> \"{dst}\";",
            space = SPACE_THERAGRAPH,
            e     = EDGE_FOLLOWS,
            src   = fwr_vid,
            dst   = fwe_vid,
        );
        self.run_write(OP, &query).await
    }

    /// Upsert user + post vertices and insert a `likes` edge.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_like", user = %user, token_id = %token_id))]
    pub async fn sync_like(
        &self,
        contract: &str,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_like";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("contract={contract} token_id={token_id}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_like(contract, token_id, user, tx_hash))
            .await
    }

    async fn inner_sync_like(
        &self,
        _contract: &str,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_like";

        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        let reaction_type = "like";
        let weight = map_reaction_weight(reaction_type);
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_likes}({p_eid}, {p_liked_at}, {p_rt}, {p_weight}) VALUES \"{user_vid}\" -> \"{post_vid}\":(\"{txh}\", now(), \"{rt}\", {wt});",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&user_vid, &user),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_likes = EDGE_LIKES,
            p_eid = PROP_EVENT_ID,
            p_liked_at = PROP_LIKED_AT,
            p_rt = PROP_REACTION_TYPE,
            p_weight = PROP_WEIGHT,
            user_vid = user_vid,
            post_vid = post_vid,
            txh = tx_hash,
            rt = reaction_type,
            wt = weight,
        );

        self.run_write(OP, &query).await?;
        info!("GraphSync: like edge written user={} post={}", user, token_id);
        Ok(())
    }

    /// Insert a `comments_on` edge.
    ///
    /// `comment_preview` is filtered to alphanumeric + space and capped at 120 chars.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_comment", user = %user, token_id = %token_id))]
    pub async fn sync_comment(
        &self,
        token_id: &str,
        user: &str,
        event_id: &str,
        comment_preview: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_comment";

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
        if !is_safe_id(event_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("event_id={event_id}"),
            });
        }

        self.with_retry(OP, || {
            self.inner_sync_comment(token_id, user, event_id, comment_preview)
        })
        .await
    }

    async fn inner_sync_comment(
        &self,
        token_id: &str,
        user: &str,
        event_id: &str,
        comment_preview: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_comment";

        let user = normalize_address(user);
        let user = user.as_str();
        let safe_preview: String = comment_preview
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ')
            .take(120)
            .collect();

        let rank = comment_rank(event_id);
        let user_vid = vid_user(user);
        let post_vid = vid_post(token_id);
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_comments}({p_eid}, {p_comment_text}, {p_commented_at}) VALUES \"{user_vid}\" -> \"{post_vid}\"@{rank}:(\"{eid}\", \"{preview}\", now());",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&user_vid, user),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_comments = EDGE_COMMENTS_ON,
            p_eid = PROP_EVENT_ID,
            p_comment_text = PROP_COMMENT_TEXT,
            p_commented_at = PROP_COMMENTED_AT,
            user_vid = user_vid,
            post_vid = post_vid,
            rank = rank,
            eid = event_id,
            preview = safe_preview,
        );

        self.run_write(OP, &query).await?;
        info!("GraphSync: comment edge written user={} post={}", user, token_id);
        Ok(())
    }

    /// Delete the `likes` edge on unlike.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_unlike", user = %user, token_id = %token_id))]
    pub async fn sync_unlike(
        &self,
        contract: &str,
        token_id: &str,
        user: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unlike";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("contract={contract} token_id={token_id}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_unlike(contract, token_id, user))
            .await
    }

    async fn inner_sync_unlike(
        &self,
        _contract: &str,
        token_id: &str,
        user: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unlike";

        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        let query = format!(
            "USE {space};\nDELETE EDGE {e_likes} \"{user_vid}\" -> \"{post_vid}\";",
            space = SPACE_THERAGRAPH,
            e_likes = EDGE_LIKES,
            user_vid = user_vid,
            post_vid = post_vid,
        );

        self.run_write(OP, &query).await?;
        info!("GraphSync: unlike edge deleted user={} post={}", user, token_id);
        Ok(())
    }

    /// Write a purchase (copy) edge to the graph.
    ///
    /// MIGRATION 15: writes to BOTH `likes` (reaction_type="purchase") AND the
    /// dedicated `purchases` edge type. MIGRATION 26 (2026-09-13, lazy-mint
    /// economy): also increments `copies_minted` on the original content's
    /// `post` vertex in the same nGQL batch — `token_id` here is the *original*
    /// content's VID (see call site's VID-FIX comment), not the new copy's.
    /// Transient Nebula failures are retried up to 3 times with exponential
    /// backoff.
    #[instrument(skip(self), fields(op = "sync_purchase", buyer = %buyer, token_id = %token_id))]
    pub async fn sync_purchase(
        &self,
        contract: &str,
        token_id: &str,
        buyer: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_purchase";

        if !is_safe_address(buyer) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("buyer={buyer}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("contract={contract} token_id={token_id}"),
            });
        }

        self.with_retry(OP, || {
            self.inner_sync_purchase(token_id, buyer, tx_hash)
        })
        .await
    }

    async fn inner_sync_purchase(
        &self,
        token_id: &str,
        buyer: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_purchase";
        let buyer = normalize_address(buyer);
        let buyer = buyer.as_str();
        let buyer_vid = vid_user(buyer);
        let post_vid = vid_post(token_id);
        let reaction_type = "purchase";
        let weight = map_reaction_weight(reaction_type);
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_likes}({p_eid}, {p_liked_at}, {p_rt}, {p_weight}) VALUES \"{buyer_vid}\" -> \"{post_vid}\":(\"{txh}\", now(), \"{rt}\", {wt});\nINSERT EDGE IF NOT EXISTS {e_purchases}({p_eid}, {p_purchased_at}, {p_weight}) VALUES \"{buyer_vid}\" -> \"{post_vid}\":(\"{txh}\", now(), {wt});\nUPDATE VERTEX ON post \"{post_vid}\" SET {p_copies_minted} = {p_copies_minted} + 1;",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&buyer_vid, buyer),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_likes = EDGE_LIKES,
            e_purchases = EDGE_PURCHASES,
            p_eid = PROP_EVENT_ID,
            p_liked_at = PROP_LIKED_AT,
            p_purchased_at = PROP_PURCHASED_AT,
            p_rt = PROP_REACTION_TYPE,
            p_weight = PROP_WEIGHT,
            p_copies_minted = PROP_COPIES_MINTED,
            buyer_vid = buyer_vid,
            post_vid = post_vid,
            txh = tx_hash,
            rt = reaction_type,
            wt = weight,
        );

        self.run_write(OP, &query).await?;
        info!("GraphSync: purchase edge written buyer={} post={}", buyer, token_id);
        Ok(())
    }
}
