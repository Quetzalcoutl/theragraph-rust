use tracing::warn;

use super::{GraphClient, GraphTransport, normalize_address, map_reaction_weight};

// Compile-time column lists for write_edge calls — identical on every invocation.
// Eliminates the String alloc that format!() would produce per write.
const FOLLOWS_EDGE_PROPS:   &str = "event_id, followed_at, weight";
const COMMENT_EDGE_PROPS:   &str = "event_id, comment_text, commented_at";
const LIKES_EDGE_PROPS:     &str = "event_id, liked_at, reaction_type, weight";
const PURCHASES_EDGE_PROPS: &str = "event_id, purchased_at, weight";
const BOOKMARK_EDGE_PROPS:  &str = "event_id, bookmarked_at";
use crate::recommendation::schema_consts::{
    vid_user, vid_post, vid_genre, comment_rank, sanitize_genre_slug,
    SPACE_THERAGRAPH,
    EDGE_FOLLOWS, EDGE_LIKES, EDGE_PURCHASES, EDGE_VIEW_EVENT, EDGE_CREATOR_AFFINITY,
    EDGE_COMMENTS_ON, EDGE_BOOKMARKED, EDGE_MUSIC_LISTEN,
    EDGE_FLIX_WATCH, EDGE_STYLE_PREFERENCE, EDGE_GENRE_PREFERENCE,
    PROP_DURATION_SECONDS, PROP_WEIGHT,
    PROP_EVENT_ID,
    PROP_EVENT_TIME, PROP_TOTAL_VIEWS,
    PROP_TOTAL_DURATION_SECS, PROP_AFFINITY_SCORE, PROP_LAST_INTERACTION_AT,
    PROP_PCT_PLAYED, PROP_LISTENED_AT,
    PROP_WATCHED_AT, PROP_REWATCH_COUNT,
    PROP_CONFIDENCE, PROP_TIME_CONTEXT, PROP_SIGNAL_SOURCE,
    is_safe_address, is_safe_id, is_safe_post_vid_id,
    ensure_user_vertex_nql, ensure_post_vertex_nql,
};

impl<T: GraphTransport> GraphClient<T> {
    /// Upsert user vertices and insert a `follows` edge.
    ///
    /// Best-effort: logs on failure, never propagates — caller must persist
    /// the canonical follow record in PostgreSQL for durability.
    #[allow(dead_code)] // NEBULA-003: graph_sync bypasses this via execute_write for retry reliability
    pub async fn write_follows_edge(&self, follower: &str, followee: &str, event_id: &str) {
        // VID-CASE-001: normalise before validating so EIP-55 checksummed addresses
        // (mixed-case hex) are accepted instead of silently dropping the edge write.
        let follower = normalize_address(follower);
        let followee = normalize_address(followee);
        if !is_safe_address(&follower) || !is_safe_address(&followee) || !is_safe_id(event_id) {
            warn!("write_follows_edge: invalid input — follower={follower} followee={followee}");
            return;
        }
        // UPSERT-HOT-001: replaced UPSERT VERTEX with INSERT VERTEX IF NOT EXISTS.
        // UPSERT reads then conditionally writes; IF NOT EXISTS is a pure conditional
        // insert — no read when the vertex already exists.  Also stops the silent
        // reset of counter properties (followers_count etc.) on every repeat event.
        // SCHEMA-FOLLOWS-TYPE: removed follows.type column — the literal "follow"
        // is always deducible from the edge type name and was never read by any query.
        let fwr_vid = vid_user(&follower);
        let fwe_vid = vid_user(&followee);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (fwr_seen, fwe_seen) = self.vertex_bloom_check(&fwr_vid, &fwe_vid).await;
        self.write_edge(
            "follows_edge",
            EDGE_FOLLOWS,
            true,
            &fwr_vid,
            if fwr_seen { String::new() } else { ensure_user_vertex_nql(&fwr_vid, &follower) },
            &fwe_vid,
            if fwe_seen { String::new() } else { ensure_user_vertex_nql(&fwe_vid, &followee) },
            None,
            FOLLOWS_EDGE_PROPS,
            &format!("\"{event_id}\", now(), 1.0"),
        ).await;
        // Invalidate all follow-related caches so the next feed open reflects the new
        // edge rather than stale data served for the full TTL (300s for following/prefs,
        // 600s for recs).  Mirrors the five invalidations in the Kafka-path handle_follow
        // (social.rs) so both write paths are complete and permanently in sync.
        if let Some(ref cache) = self.cache {
            cache.delete_following(&follower).await;
            cache.delete_user_prefs(&follower).await;
            cache.delete_recommendations(&follower).await;
            cache.delete_fof_all(&follower).await;
            cache.delete_user_suggestions(&follower, &followee).await;
        }
        self.mark_vertices_seen(fwr_vid, fwe_vid);
    }

    /// Delete the `follows` edge on unfollow.
    #[allow(dead_code)] // NEBULA-003: same bypass as write_follows_edge; available for future HTTP API
    pub async fn delete_follows_edge(&self, follower: &str, followee: &str) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let follower = normalize_address(follower);
        let followee = normalize_address(followee);
        if !is_safe_address(&follower) || !is_safe_address(&followee) {
            warn!("delete_follows_edge: invalid input — follower={follower} followee={followee}");
            return;
        }
        let fwr_vid = vid_user(&follower);
        let fwe_vid = vid_user(&followee);
        self.delete_edge("delete_follows_edge", EDGE_FOLLOWS, &fwr_vid, &fwe_vid).await;
        // Mirrors the five invalidations in write_follows_edge — unfollow changes
        // the same graph topology, so all five caches must be cleared here too.
        if let Some(ref cache) = self.cache {
            cache.delete_following(&follower).await;
            cache.delete_user_prefs(&follower).await;
            cache.delete_recommendations(&follower).await;
            cache.delete_fof_all(&follower).await;
            cache.delete_user_suggestions(&follower, &followee).await;
        }
    }

    /// Upsert a `view_event` edge — accumulates dwell time across repeated views.
    ///
    /// Uses UPSERT (rank 0) so each (viewer, post) pair has exactly one edge.
    /// `duration_seconds` accumulates: a user who watches 3 times gets the total
    /// dwell recorded, not just the most-recent view. This bounds storage at
    /// O(users × posts_viewed) and makes `sum(duration_seconds)` in FoF queries
    /// return total dwell rather than the last view's duration.
    ///
    /// Only writes when duration_seconds > 0 so zero-dwell noise is filtered.
    /// Best-effort: never propagates errors to caller.
    pub async fn write_view_event(
        &self,
        viewer: &str,
        post_id: &str,
        event_id: &str,
        duration_seconds: u32,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let viewer = normalize_address(viewer);
        if !is_safe_address(&viewer)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
        {
            warn!("write_view_event: invalid input — viewer={viewer} post_id={post_id}");
            return;
        }
        if duration_seconds == 0 {
            return;
        }
        // UPSERT-HOT-001 / A-04: vertex upserts via ensure_*_vertex_nql helper.
        let vwr_vid = vid_user(&viewer);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (vwr_seen, pid_seen) = self.vertex_bloom_check(&vwr_vid, &pid_vid).await;
        let query = format!(
            "USE {space};\n{upsert_vwr}\n{upsert_pid}\nUPSERT EDGE ON {e_view_event} \"{vwr_vid}\" -> \"{pid_vid}\"\nSET {p_eid} = \"{eid}\",\n    {p_event_time} = now(),\n    {p_dur} = {p_dur} + {dur};",
            space = SPACE_THERAGRAPH,
            upsert_vwr = if vwr_seen { String::new() } else { ensure_user_vertex_nql(&vwr_vid, &viewer) },
            upsert_pid = if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            vwr_vid = vwr_vid,
            pid_vid = pid_vid,
            e_view_event = EDGE_VIEW_EVENT,
            eid = event_id,
            dur = duration_seconds,
            p_eid = PROP_EVENT_ID,
            p_event_time = PROP_EVENT_TIME,
            p_dur = PROP_DURATION_SECONDS,
        );
        self.execute_write_or_dlq("view_event_edge", &viewer, post_id, &query).await;
        self.mark_vertices_seen(vwr_vid, pid_vid);
    }

    /// UPSERT the `creator_affinity` edge, accumulating view count and dwell time.
    ///
    /// Uses NebulaGraph UPSERT semantics: on first insert defaults are applied,
    /// then SET expressions reference the current (now-defaulted) values so the
    /// counters accumulate correctly across repeated calls.
    pub async fn write_creator_affinity(
        &self,
        viewer: &str,
        creator: &str,
        view_duration_secs: u32,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let viewer = normalize_address(viewer);
        let creator = normalize_address(creator);
        if !is_safe_address(&viewer) || !is_safe_address(&creator) {
            warn!("write_creator_affinity: invalid input — viewer={viewer} creator={creator}");
            return;
        }
        if viewer == creator {
            return;
        }
        // affinity_score uses exponential decay toward a 0-10 ceiling.
        // Old formula (total_secs / 300) grew unboundedly — a user who watched
        // 500 minutes accumulated affinity_score=100, making early interactions
        // dwarf everything else. New formula: score = 10 * (1 - e^(-total/1800))
        // which saturates at 10.0 after ~30 min of total watch time, never
        // exceeds 10.0, and is monotonically increasing.
        // The SET clause evaluates after the accumulation, so total_duration_secs
        // already includes {dur} when affinity_score is recomputed.
        // UPSERT-HOT-001 / A-04: vertex upserts via ensure_user_vertex_nql helper.
        let vwr_vid = vid_user(&viewer);
        let ctr_vid = vid_user(&creator);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (vwr_seen, ctr_seen) = self.vertex_bloom_check(&vwr_vid, &ctr_vid).await;
        let query = format!(
            "USE {space};\n{upsert_vwr}\n{upsert_ctr}\nUPSERT EDGE ON {e_creator_affinity} \"{vwr_vid}\" -> \"{ctr_vid}\"\nSET {p_total_views} = {p_total_views} + 1,\n    {p_dur_secs} = {p_dur_secs} + {dur},\n    {p_affinity} = 10.0 * (1.0 - exp(-(toFloat({p_dur_secs} + {dur}) / 1800.0))),\n    {p_last_at} = now();",
            space = SPACE_THERAGRAPH,
            upsert_vwr = if vwr_seen { String::new() } else { ensure_user_vertex_nql(&vwr_vid, &viewer) },
            upsert_ctr = if ctr_seen { String::new() } else { ensure_user_vertex_nql(&ctr_vid, &creator) },
            vwr_vid = vwr_vid,
            ctr_vid = ctr_vid,
            e_creator_affinity = EDGE_CREATOR_AFFINITY,
            dur = view_duration_secs,
            p_total_views = PROP_TOTAL_VIEWS,
            p_dur_secs = PROP_TOTAL_DURATION_SECS,
            p_affinity = PROP_AFFINITY_SCORE,
            p_last_at = PROP_LAST_INTERACTION_AT,
        );
        self.execute_write_or_dlq("creator_affinity_edge", &viewer, &creator, &query).await;
        self.mark_vertices_seen(vwr_vid, ctr_vid);
    }

    /// UPSERT a `music_listen` edge — accumulates duration and pct_played.
    ///
    /// UPSERT semantics: one edge per (listener, post). duration_seconds sums
    /// across repeated listens. pct_played uses a running weighted geometric mean
    /// so full plays dominate over skips (Raph Levien). 10-day half-life in FoF.
    ///
    /// Rate limiting: caller (dispatch_graph_interaction) checks Redis key
    /// `listen:{user}:{nft_id}` with 1h TTL — skips Nebula write on cache hit
    /// to prevent loop listeners from inflating graph edges (Spotify engineers).
    pub async fn write_music_listen_edge(
        &self,
        listener: &str,
        post_id: &str,
        event_id: &str,
        duration_seconds: u32,
        pct_played: f32,
    ) {
        let listener = normalize_address(listener);
        if !is_safe_address(&listener)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
        {
            warn!("write_music_listen_edge: invalid input — listener={listener} post_id={post_id}");
            return;
        }
        if duration_seconds == 0 {
            return;
        }
        let pct = pct_played.clamp(0.0, 1.0);
        let lsn_vid = vid_user(&listener);
        let pid_vid = vid_post(post_id);
        let (lsn_seen, pid_seen) = self.vertex_bloom_check(&lsn_vid, &pid_vid).await;
        // Geometric mean update for pct_played: new = sqrt(old * pct) — dampens
        // the influence of any single play, prevents full-plays from saturating to 1.0
        // after 2 loops, and keeps skips from dragging quality to 0.0 permanently.
        let query = format!(
            "USE {space};\n{upsert_lsn}\n{upsert_pid}\nUPSERT EDGE ON {e_ml} \"{lsn_vid}\" -> \"{pid_vid}\"\nSET {p_eid} = \"{eid}\",\n    {p_listened_at} = now(),\n    {p_dur} = {p_dur} + {dur},\n    {p_pct} = sqrt({p_pct} * {pct:.4}),\n    {p_weight} = 1.0;",
            space = SPACE_THERAGRAPH,
            upsert_lsn = if lsn_seen { String::new() } else { ensure_user_vertex_nql(&lsn_vid, &listener) },
            upsert_pid = if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            lsn_vid = lsn_vid,
            pid_vid = pid_vid,
            e_ml = EDGE_MUSIC_LISTEN,
            eid = event_id,
            dur = duration_seconds,
            pct = pct,
            p_eid = PROP_EVENT_ID,
            p_listened_at = PROP_LISTENED_AT,
            p_dur = PROP_DURATION_SECONDS,
            p_pct = PROP_PCT_PLAYED,
            p_weight = PROP_WEIGHT,
        );
        self.execute_write_or_dlq("music_listen_edge", &listener, post_id, &query).await;
        self.mark_vertices_seen(lsn_vid, pid_vid);
    }

    pub async fn write_flix_watch_edge(
        &self,
        viewer: &str,
        post_id: &str,
        event_id: &str,
        duration_seconds: u32,
        pct_played: f32,
    ) {
        let viewer  = normalize_address(viewer);
        let post_id = post_id.trim();
        if !is_safe_address(&viewer) || !is_safe_id(post_id) {
            warn!("write_flix_watch_edge: invalid input — viewer={viewer} post={post_id}");
            return;
        }
        let dur = duration_seconds.min(7200);
        let pct = pct_played.clamp(0.0, 1.0);
        let v_vid = vid_user(&viewer);
        let p_vid = vid_post(post_id);
        let (v_seen, p_seen) = self.vertex_bloom_check(&v_vid, &p_vid).await;
        let query = format!(
            "USE {space};\n{upsert_v}\n{upsert_p}\nUPSERT EDGE ON {e_fw} \"{v_vid}\" -> \"{p_vid}\"\nSET {p_eid} = \"{eid}\",\n    {p_wat} = now(),\n    {p_dur} = {p_dur} + {dur},\n    {p_pct} = sqrt({p_pct} * {pct:.4}),\n    {p_rwc} = {p_rwc} + 1,\n    {p_weight} = 1.0;",
            space    = SPACE_THERAGRAPH,
            upsert_v = if v_seen { String::new() } else { ensure_user_vertex_nql(&v_vid, &viewer) },
            upsert_p = if p_seen { String::new() } else { ensure_post_vertex_nql(&p_vid, post_id) },
            v_vid    = v_vid,
            p_vid    = p_vid,
            e_fw     = EDGE_FLIX_WATCH,
            eid      = event_id,
            dur      = dur,
            pct      = pct,
            p_eid    = PROP_EVENT_ID,
            p_wat    = PROP_WATCHED_AT,
            p_dur    = PROP_DURATION_SECONDS,
            p_pct    = PROP_PCT_PLAYED,
            p_rwc    = PROP_REWATCH_COUNT,
            p_weight = PROP_WEIGHT,
        );
        self.execute_write_or_dlq("flix_watch_edge", &viewer, post_id, &query).await;
        self.mark_vertices_seen(v_vid, p_vid);
    }

    /// Accumulate `creator_affinity` from a music listen.
    ///
    /// Uses geometric mean for affinity_score accumulation so loop listeners
    /// (same creator 50× in a day) don't saturate the score to 10.0 after 2 plays
    /// (Raph Levien). pct_played quality multiplier: full listen = 1.0×, skip = 0.12×.
    pub async fn write_music_creator_affinity(
        &self,
        listener: &str,
        creator: &str,
        duration_seconds: u32,
        pct_played: f32,
    ) {
        let listener = normalize_address(listener);
        let creator = normalize_address(creator);
        if !is_safe_address(&listener) || !is_safe_address(&creator) {
            warn!("write_music_creator_affinity: invalid input — listener={listener} creator={creator}");
            return;
        }
        if listener == creator {
            return;
        }
        let pct = pct_played.clamp(0.0, 1.0);
        // Quality-weighted duration: a full listen contributes 100% of duration_seconds;
        // a 12% skip contributes only 12%. Matches Spotify's approach of weighting
        // listen quality into creator affinity rather than raw play counts.
        let quality_secs = ((duration_seconds as f32) * pct) as u32;
        let lsn_vid = vid_user(&listener);
        let ctr_vid = vid_user(&creator);
        let (lsn_seen, ctr_seen) = self.vertex_bloom_check(&lsn_vid, &ctr_vid).await;
        let query = format!(
            "USE {space};\n{upsert_lsn}\n{upsert_ctr}\nUPSERT EDGE ON {e_ca} \"{lsn_vid}\" -> \"{ctr_vid}\"\nSET {p_total_views} = {p_total_views} + 1,\n    {p_dur_secs} = {p_dur_secs} + {qdur},\n    {p_affinity} = 10.0 * (1.0 - exp(-(toFloat({p_dur_secs} + {qdur}) / 1800.0))),\n    {p_last_at} = now();",
            space = SPACE_THERAGRAPH,
            upsert_lsn = if lsn_seen { String::new() } else { ensure_user_vertex_nql(&lsn_vid, &listener) },
            upsert_ctr = if ctr_seen { String::new() } else { ensure_user_vertex_nql(&ctr_vid, &creator) },
            lsn_vid = lsn_vid,
            ctr_vid = ctr_vid,
            e_ca = EDGE_CREATOR_AFFINITY,
            qdur = quality_secs,
            p_total_views = PROP_TOTAL_VIEWS,
            p_dur_secs = PROP_TOTAL_DURATION_SECS,
            p_affinity = PROP_AFFINITY_SCORE,
            p_last_at = PROP_LAST_INTERACTION_AT,
        );
        self.execute_write_or_dlq("music_creator_affinity_edge", &listener, &creator, &query).await;
        self.mark_vertices_seen(lsn_vid, ctr_vid);
    }

    /// Write style_preference + genre_preference edges from a companion signal.
    ///
    /// UPSERT semantics: confidence = old * 0.7 + extracted * 0.3 (EMA).
    /// Companion source decays at 3-day half-life (vs listen's 10-day).
    pub async fn write_companion_preference_edges(
        &self,
        user: &str,
        style_tags: &[String],
        genre_ids: &[i32],
        confidence: f32,
        time_context: &str,
    ) {
        let user = normalize_address(user);
        if !is_safe_address(&user) {
            warn!("write_companion_preference_edges: invalid user={user}");
            return;
        }
        let conf = confidence.clamp(0.0, 1.0);
        let usr_vid = vid_user(&user);
        let (usr_seen, _) = self.vertex_bloom_check(&usr_vid, &usr_vid).await;
        let upsert_usr = if usr_seen { String::new() } else { ensure_user_vertex_nql(&usr_vid, &user) };

        for tag in style_tags {
            // tag VID: "style:{tag}"
            let tag_safe: String = tag.chars().filter(|c| c.is_alphanumeric() || *c == '-').collect();
            if tag_safe.is_empty() || tag_safe.len() > 32 { continue; }
            let tag_vid = format!("style:{tag_safe}");
            let query = format!(
                "USE {space};\n{upsert_usr}UPSERT EDGE ON {e_style} \"{usr_vid}\" -> \"{tag_vid}\" \
                 SET {p_source} = \"companion\", \
                     {p_confidence} = CASE WHEN exists({p_confidence}) \
                         THEN {p_confidence} * 0.7 + {conf:.4} * 0.3 \
                         ELSE {conf:.4} END, \
                     {p_time_ctx} = \"{time_ctx}\", \
                     updated_at = now();",
                space = SPACE_THERAGRAPH,
                e_style = EDGE_STYLE_PREFERENCE,
                p_source = PROP_SIGNAL_SOURCE,
                p_confidence = PROP_CONFIDENCE,
                p_time_ctx = PROP_TIME_CONTEXT,
                time_ctx = time_context.replace('"', ""),
                conf = conf,
                usr_vid = usr_vid,
                tag_vid = tag_vid,
                upsert_usr = upsert_usr,
            );
            self.execute_write_or_dlq("companion_style_edge", &user, &tag_vid, &query).await;
        }

        for genre_id in genre_ids {
            let genre_vid = format!("genre:{genre_id}");
            let query = format!(
                "USE {space};\n{upsert_usr}UPSERT EDGE ON {e_genre} \"{usr_vid}\" -> \"{genre_vid}\" \
                 SET {p_source} = \"companion\", \
                     {p_confidence} = CASE WHEN exists({p_confidence}) \
                         THEN {p_confidence} * 0.7 + {conf:.4} * 0.3 \
                         ELSE {conf:.4} END, \
                     updated_at = now();",
                space = SPACE_THERAGRAPH,
                e_genre = EDGE_GENRE_PREFERENCE,
                p_source = PROP_SIGNAL_SOURCE,
                p_confidence = PROP_CONFIDENCE,
                conf = conf,
                usr_vid = usr_vid,
                genre_vid = genre_vid,
                upsert_usr = upsert_usr,
            );
            self.execute_write_or_dlq("companion_genre_edge", &user, &genre_vid, &query).await;
        }

        self.mark_vertices_seen(usr_vid, String::new());
    }

    /// Write `genre_preference` edges for a user's explicitly declared
    /// favorite genres (up to 30 kebab-case slugs).
    ///
    /// GENRE-01: distinct from `write_companion_preference_edges`'s
    /// `genre_ids` handling, which is a narrow (max 3), AI-chat-derived signal
    /// using the old opaque numeric `genre:{id}` VID format. This is the wide
    /// (≤30), user-declared preference path using the canonical kebab-case
    /// slug VID format `genre:{slug}` — the same slug namespace NFT content
    /// tags use (see `event_processor::elixir_db::process_enrichment`).
    ///
    /// Same UPSERT + EMA-confidence semantics as `write_companion_preference_edges`
    /// (`new = old * 0.7 + 1.0 * 0.3`, source = "declared") so re-submitting an
    /// unchanged preference list reinforces rather than flickers. Callers must
    /// cap `genre_slugs` at 30 before calling — this method does not
    /// re-truncate, matching `write_companion_preference_edges`'s contract
    /// (validation lives at the API boundary). Each slug is re-sanitized via
    /// `sanitize_genre_slug` before being embedded in a VID; slugs that
    /// sanitize to empty are skipped. Best-effort; never propagates errors.
    pub async fn write_genre_preference_edges(&self, user: &str, genre_slugs: &[String]) {
        let user = normalize_address(user);
        if !is_safe_address(&user) {
            warn!("write_genre_preference_edges: invalid user={user}");
            return;
        }
        let usr_vid = vid_user(&user);
        let (usr_seen, _) = self.vertex_bloom_check(&usr_vid, &usr_vid).await;
        let upsert_usr = if usr_seen { String::new() } else { ensure_user_vertex_nql(&usr_vid, &user) };

        for slug in genre_slugs {
            let Some(safe_slug) = sanitize_genre_slug(slug) else { continue };
            let genre_vid = vid_genre(&safe_slug);
            let query = format!(
                "USE {space};\n{upsert_usr}UPSERT EDGE ON {e_genre} \"{usr_vid}\" -> \"{genre_vid}\" \
                 SET {p_source} = \"declared\", \
                     {p_confidence} = CASE WHEN exists({p_confidence}) \
                         THEN {p_confidence} * 0.7 + 1.0 * 0.3 \
                         ELSE 1.0 END, \
                     updated_at = now();",
                space = SPACE_THERAGRAPH,
                e_genre = EDGE_GENRE_PREFERENCE,
                p_source = PROP_SIGNAL_SOURCE,
                p_confidence = PROP_CONFIDENCE,
                usr_vid = usr_vid,
                genre_vid = genre_vid,
                upsert_usr = upsert_usr,
            );
            self.execute_write_or_dlq("genre_preference_edge", &user, &genre_vid, &query).await;
        }

        self.mark_vertices_seen(usr_vid, String::new());
    }

    /// Insert a `comments_on` edge.
    pub async fn write_comments_on(
        &self,
        commenter: &str,
        post_id: &str,
        event_id: &str,
        comment_preview: &str,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let commenter = normalize_address(commenter);
        if !is_safe_address(&commenter)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
        {
            warn!("write_comments_on: invalid input — commenter={commenter} post_id={post_id}");
            return;
        }
        // Truncate preview to 120 chars and strip any characters that would break nGQL strings.
        let safe_preview: String = comment_preview
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ')
            .take(120)
            .collect();
        // NEBULA-002: shared with graph_sync.rs's inner_sync_comment via
        // schema_consts::comment_rank — see its doc comment for why (dash
        // stripping for UUID event_ids, 15-digit cap to avoid i64 overflow).
        // The two call sites previously hand-derived this independently and
        // drifted, leaving this REST path with a stale, buggy version.
        let rank = comment_rank(event_id);
        // UPSERT-HOT-001 / A-04: vertex upserts via ensure_*_vertex_nql helper.
        let cmtr_vid = vid_user(&commenter);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (cmtr_seen, pid_seen) = self.vertex_bloom_check(&cmtr_vid, &pid_vid).await;
        self.write_edge(
            "comments_on_edge",
            EDGE_COMMENTS_ON,
            false,
            &cmtr_vid,
            if cmtr_seen { String::new() } else { ensure_user_vertex_nql(&cmtr_vid, &commenter) },
            &pid_vid,
            if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            Some(rank),
            COMMENT_EDGE_PROPS,
            &format!("\"{event_id}\", \"{safe_preview}\", now()"),
        ).await;
        self.mark_vertices_seen(cmtr_vid, pid_vid);
    }

    /// Upsert user + post vertices and insert a `likes` edge.
    pub async fn write_likes_edge(
        &self,
        liker: &str,
        post_id: &str,
        event_id: &str,
        reaction_type: &str,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let liker = normalize_address(liker);
        if !is_safe_address(&liker)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
            || !is_safe_id(reaction_type)
        {
            warn!("write_likes_edge: invalid input — liker={liker} post_id={post_id} reaction_type={reaction_type}");
            return;
        }
        // Map reaction_type to edge weight via shared function so the Kafka event
        // processor and API interaction handler both produce consistent weights.
        let weight = map_reaction_weight(reaction_type);
        // UPSERT-HOT-001 / A-04: vertex upserts via ensure_*_vertex_nql helper.
        let lkr_vid = vid_user(&liker);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (lkr_seen, pid_seen) = self.vertex_bloom_check(&lkr_vid, &pid_vid).await;
        self.write_edge(
            "likes_edge",
            EDGE_LIKES,
            true,
            &lkr_vid,
            if lkr_seen { String::new() } else { ensure_user_vertex_nql(&lkr_vid, &liker) },
            &pid_vid,
            if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            None,
            LIKES_EDGE_PROPS,
            &format!("\"{event_id}\", now(), \"{reaction_type}\", {weight}"),
        ).await;
        self.mark_vertices_seen(lkr_vid, pid_vid);
    }

    /// Write a dedicated `purchases` edge (migration 15).
    ///
    /// Separate from `write_likes_edge` so the purchases edge type carries only
    /// purchase events and `get_purchase_fof_recommendations` can traverse it at
    /// storaged level without a graphd-side WHERE filter on reaction_type.
    ///
    /// Best-effort: never propagates errors to the caller.
    pub async fn write_purchases_edge(
        &self,
        buyer: &str,
        post_id: &str,
        event_id: &str,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let buyer = normalize_address(buyer);
        if !is_safe_address(&buyer)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
        {
            warn!("write_purchases_edge: invalid input — buyer={buyer} post_id={post_id}");
            return;
        }
        let buyer_vid = vid_user(&buyer);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (buyer_seen, pid_seen) = self.vertex_bloom_check(&buyer_vid, &pid_vid).await;
        self.write_edge(
            "purchases_edge",
            EDGE_PURCHASES,
            false,
            &buyer_vid,
            if buyer_seen { String::new() } else { ensure_user_vertex_nql(&buyer_vid, &buyer) },
            &pid_vid,
            if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            None,
            PURCHASES_EDGE_PROPS,
            &format!("\"{event_id}\", now(), 2.0"),
        ).await;
        // Invalidate the buyer's FoF and recommendation caches so their followers
        // see the purchase signal on the next feed open rather than after the full TTL.
        if let Some(ref cache) = self.cache {
            cache.delete_fof_all(&buyer).await;
            cache.delete_recommendations(&buyer).await;
        }
        self.mark_vertices_seen(buyer_vid, pid_vid);
    }

    /// Write a `bookmarked` edge when a user saves content via the REST API.
    ///
    /// S30-05: InteractionType::Save fell to the `_ => {}` arm — no Nebula write
    /// happened for bookmark events submitted through the API interaction endpoint.
    /// This mirrors graph_sync::sync_bookmark: IF NOT EXISTS preserves the original
    /// bookmarked_at timestamp on duplicate save events (idempotent).
    pub async fn write_bookmark_edge(&self, user: &str, post_id: &str, event_id: &str) {
        let user = normalize_address(user);
        if !is_safe_address(&user)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
        {
            warn!("write_bookmark_edge: invalid input — user={user} post_id={post_id}");
            return;
        }
        let usr_vid = vid_user(&user);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (usr_seen, pid_seen) = self.vertex_bloom_check(&usr_vid, &pid_vid).await;
        self.write_edge(
            "bookmarked_edge",
            EDGE_BOOKMARKED,
            true,
            &usr_vid,
            if usr_seen { String::new() } else { ensure_user_vertex_nql(&usr_vid, &user) },
            &pid_vid,
            if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            None,
            BOOKMARK_EDGE_PROPS,
            &format!("\"{event_id}\", now()"),
        ).await;
        self.mark_vertices_seen(usr_vid, pid_vid);
    }

    /// Delete the `bookmarked` edge when a user removes a save via the REST API.
    pub async fn delete_bookmark_edge(&self, user: &str, post_id: &str) {
        let user = normalize_address(user);
        if !is_safe_address(&user) || !is_safe_post_vid_id(post_id) {
            warn!("delete_bookmark_edge: invalid input — user={user} post_id={post_id}");
            return;
        }
        let usr_vid = vid_user(&user);
        let pid_vid = vid_post(post_id);
        self.delete_edge("delete_bookmark_edge", EDGE_BOOKMARKED, &usr_vid, &pid_vid).await;
    }
}
