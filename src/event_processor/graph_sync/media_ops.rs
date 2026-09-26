//! Media consumption operations: music listen.

use crate::recommendation::graph_client::{GraphTransport, normalize_address};
use crate::recommendation::schema_consts::{
    SPACE_THERAGRAPH, EDGE_MUSIC_LISTEN,
    PROP_EVENT_ID, PROP_WEIGHT, PROP_DURATION_SECONDS, PROP_PCT_PLAYED, PROP_LISTENED_AT,
    vid_user, vid_post,
    is_safe_address, is_safe_id, is_safe_post_vid_id,
    ensure_user_vertex_nql, ensure_post_vertex_nql,
};
use tracing::{info, instrument};

use super::{GraphSync, GraphSyncError};

impl<T: GraphTransport> GraphSync<T> {
    /// UPSERT the `music_listen` edge for a confirmed listen event (≥ 30s).
    ///
    /// Called from the Kafka event processor when a `MusicListened` event arrives.
    /// Accumulates duration_seconds and pct_played via geometric mean so loop
    /// listeners don't saturate the edge properties (Raph Levien / Carl Lerche).
    #[allow(dead_code)]
    #[instrument(skip(self), fields(op = "sync_music_listen", user = %user, token_id = %token_id))]
    pub async fn sync_music_listen(
        &self,
        token_id: &str,
        user: &str,
        event_id: &str,
        duration_seconds: u32,
        pct_played: f32,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_music_listen";

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
        if duration_seconds == 0 {
            return Ok(());
        }

        self.with_retry(OP, || {
            self.inner_sync_music_listen(token_id, user, event_id, duration_seconds, pct_played)
        }).await
    }

    #[allow(dead_code)]
    async fn inner_sync_music_listen(
        &self,
        token_id: &str,
        user: &str,
        event_id: &str,
        duration_seconds: u32,
        pct_played: f32,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_music_listen";
        let user = normalize_address(user);
        let pct = pct_played.clamp(0.0, 1.0);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        // Geometric mean update for pct_played: new = sqrt(old * pct).
        // duration_seconds accumulates; pct_played converges toward the typical
        // completion rate rather than being dominated by the most recent play.
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nUPSERT EDGE ON {e_ml} \"{user_vid}\" -> \"{post_vid}\"\nSET {p_eid} = \"{eid}\",\n    {p_listened_at} = now(),\n    {p_dur} = {p_dur} + {dur},\n    {p_pct} = sqrt({p_pct} * {pct:.4}),\n    {p_weight} = 1.0;",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&user_vid, &user),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_ml = EDGE_MUSIC_LISTEN,
            user_vid = user_vid,
            post_vid = post_vid,
            eid = event_id,
            dur = duration_seconds,
            pct = pct,
            p_eid = PROP_EVENT_ID,
            p_listened_at = PROP_LISTENED_AT,
            p_dur = PROP_DURATION_SECONDS,
            p_pct = PROP_PCT_PLAYED,
            p_weight = PROP_WEIGHT,
        );

        self.run_write(OP, &query).await?;
        info!("GraphSync: music_listen edge written user={} post={}", user, token_id);
        Ok(())
    }
}
