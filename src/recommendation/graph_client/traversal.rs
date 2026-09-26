use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use tracing::{debug, error, warn};

use super::{GraphClient, GraphTransport, FofBucket, normalize_address};
use crate::recommendation::graph_transport::parse_nebula_table;
use crate::recommendation::schema_consts::{
    vid_user, vid_post,
    SPACE_THERAGRAPH,
    EDGE_FOLLOWS, EDGE_LIKES, EDGE_PURCHASES, EDGE_VIEW_EVENT, EDGE_CREATOR_AFFINITY,
    EDGE_COMMENTS_ON, EDGE_BOOKMARKED, EDGE_SHARED, EDGE_RECOMMENDED_TO, EDGE_FLIX_WATCH,
    EDGE_STYLE_PREFERENCE, EDGE_GENRE_PREFERENCE,
    PROP_WEIGHT, PROP_LIKED_AT, PROP_REACTION_TYPE, PROP_DURATION_SECONDS, PROP_EVENT_TIME,
    PROP_COMMENTED_AT, PROP_PURCHASED_AT, PROP_SHARED_AT, PROP_BOOKMARKED_AT, PROP_WATCHED_AT,
    PROP_SCORE, PROP_SERVED, PROP_COMPUTED_AT, PROP_CONFIDENCE,
    is_safe_address, is_safe_post_vid_id,
};

const COUNT_EXTRA_AGG: &str = "        count($-.f_vid) AS interaction_count,\n";
const COUNT_SCORE_EXPR: &str = "toFloat($-.interaction_count) * toFloat($-.friend_count)";

/// Score formula variants for `fof_traverse_simple`.
enum FofSimpleScore {
    /// score = friend_count × multiplier × exp(-age/hl)
    Weighted(f64),
    /// score = interaction_count × friend_count × exp(-age/hl)
    /// (comment-style: interaction_count = count of edge rows, friend_count = count of distinct friends)
    CountWeighted,
}

/// Configuration for the parameterised simple-score FoF helper.
struct FofSimpleConfig {
    fn_name:   &'static str,
    edge_type: &'static str,
    ts_prop:   &'static str,
    ts_alias:  &'static str,
    half_life: f64,
    score:     FofSimpleScore,
    bucket:    FofBucket,
}

/// Build the per-user nGQL body for `recommended_to` edges (no `USE` statement).
///
/// Returns `None` when the address is invalid or no NFT IDs pass validation —
/// callers must skip those users rather than emitting an empty body.
///
/// P3-04: one user-vertex INSERT outside the loop.
/// P3-05: pre-allocated String with capacity estimate (130 + N×540 bytes).
/// Min-max normalisation is applied per-user so relative ranking is preserved.
fn build_recommended_to_body(user_address: &str, served: &[(Box<str>, f32)]) -> Option<String> {
    // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
    let user_address = normalize_address(user_address);
    let user_address = user_address.as_str();
    if !is_safe_address(user_address) || served.is_empty() {
        return None;
    }
    let mut buf = String::with_capacity(130 + served.len() * 540);

    let user_vid = vid_user(user_address);
    let _ = write!(
        buf,
        "INSERT VERTEX IF NOT EXISTS user(id, username, followers_count, following_count, total_likes_given, total_posts) VALUES \"{user_vid}\":(\"{addr}\", \"\", 0, 0, 0, 0);\n",
        user_vid = user_vid,
        addr = user_address,
    );

    // Min-max normalize scores within this batch so the recommended_to edge
    // preserves relative ranking. Clamping to [0,1] would flatten the top ~20-30%
    // of scores to 1.0 (since FoF scores >> 1.0 are common), destroying the
    // differentiation needed by the feedback loop to learn which rank drove engagement.
    let max_score = served.iter().map(|(_, s)| *s).fold(f32::MIN, f32::max);
    let min_score = served.iter().map(|(_, s)| *s).fold(f32::MAX, f32::min);
    let score_range = max_score - min_score;

    let mut has_valid = false;
    for (nft_id, score) in served {
        if !is_safe_post_vid_id(nft_id) {
            continue;
        }
        has_valid = true;
        // When all items share the same score (common after FoF clamping), store 1.0
        // rather than 0.0 (what EPSILON division would produce). 0.0 tells the
        // feedback loop "lowest quality ever shown", corrupting future ranking.
        let sc = if score_range < f32::EPSILON {
            1.0f32
        } else {
            ((score - min_score) / score_range).clamp(0.0, 1.0)
        };
        let nft_vid = vid_post(nft_id);
        // F01: INSERT VERTEX IF NOT EXISTS — never overwrites real user/post data.
        // F02: Split into INSERT (new edges, served=false) + conditional UPDATE
        // (existing edges, score/time only, WHEN served==false).
        let _ = write!(
            buf,
            "INSERT VERTEX IF NOT EXISTS post(id, content, author_id, views, likes, hashtags, content_type) VALUES \"{nft_vid}\":(\"{nft}\", \"\", \"\", 0, 0, \"\", \"\");\n\
             INSERT EDGE IF NOT EXISTS {e_rec_to}({p_score}, {p_served}, {p_computed_at}) VALUES \"{user_vid}\" -> \"{nft_vid}\"@0:({sc}, false, now());\n\
             UPDATE EDGE ON {e_rec_to} \"{user_vid}\" -> \"{nft_vid}\"@0 SET {p_score} = {sc}, {p_computed_at} = now() WHEN {e_rec_to}.{p_served} == false YIELD {e_rec_to}.{p_served} AS served;\n",
            nft_vid = nft_vid,
            nft = nft_id,
            user_vid = user_vid,
            e_rec_to = EDGE_RECOMMENDED_TO,
            sc = sc,
            p_score = PROP_SCORE,
            p_served = PROP_SERVED,
            p_computed_at = PROP_COMPUTED_AT,
        );
    }

    if has_valid { Some(buf) } else { None }
}

/// Normalize `raw` to lowercase hex, then validate with `is_safe_address`.
///
/// Every traversal method needs this exact 3-step preamble (S30-09 fix).
/// Returns the normalized string on success, bails with a contextual message on failure.
fn checked_address(raw: &str, fn_name: &str) -> anyhow::Result<String> {
    let addr = normalize_address(raw);
    anyhow::ensure!(
        is_safe_address(addr.as_str()),
        "{fn_name}: invalid address: {addr}"
    );
    Ok(addr)
}

impl<T: GraphTransport> GraphClient<T> {
    // ── FoF traversal helpers ─────────────────────────────────────────────────

    /// Common 5-step scaffold shared by the three FoF methods:
    /// (1) cache read, (2) execute_query, (3) parse, (4) cache write, (5) return.
    ///
    /// Each public method validates and normalises `user_address`, builds its
    /// specific nGQL query, then delegates here.
    pub(super) async fn fof_traverse(
        &self,
        user_address: &str,
        query: &str,
        bucket: FofBucket,
    ) -> Result<Vec<(String, f64)>> {
        if let Some(ref cache) = self.cache {
            if let Some(results) = cache.get_fof_recs(bucket, user_address).await {
                debug!("FoF cache HIT for {}", user_address);
                metrics::counter!("nebula_fof_cache_hits_total").increment(1);
                // Upcast f32→f64: Redis stores f32 (half the JSON bytes); public API
                // returns f64 for the updater pre-warm path. Engine reads f32 directly
                // via cache.get_fof_recs() and never calls fof_traverse.
                return Ok(results.into_vec().into_iter().map(|(k, v)| (k.into(), v as f64)).collect());
            }
        }

        // Use execute_query_uncached to bypass the nGQL query-string cache (tier-2).
        // delete_fof_all clears the per-user FoF slot (tier-1) on follow/unfollow,
        // but execute_query has its own query-string cache that would re-promote stale
        // data for up to the nGQL TTL (~5 min), silently defeating the invalidation.
        let output = self.execute_query_uncached(query).await?;
        let results = parse_nebula_table(&output, 1, 2);

        if let Some(ref cache) = self.cache {
            cache.set_fof_recs(bucket, user_address, &results).await;
        }

        Ok(results)
    }

    /// ByteGraph FoF traversal — friends-of-friends who liked the same content.
    ///
    /// Changji Li / Hongzhi Chen pattern: multi-hop walk + temporal decay scoring.
    /// Score = (fof_count × 2 + friend_count × 1.5) × avg_engagement × exp(-age_days/7)
    ///
    /// Per-signal exponential decay: likes use a 7-day half-life. A like from 7 days ago
    /// retains e^(-1) ≈ 37% signal weight; harmonic decay gave ~12.5% by comparison,
    /// making the like queue fade too fast. ByteDance/TikTok tuning: purchases 30d,
    /// likes 7d, views 3d, comments 14d — each matched to the real-world shelf life
    /// of that intent signal.
    pub async fn get_fof_recommendations(&self, user_address: &str) -> Result<Vec<(String, f64)>> {
        let user_address = checked_address(user_address, "get_fof_recommendations")?;
        let user_address = user_address.as_str();

        // P3-03: collapse (fof, f) pairs → (fof, friend_count) BEFORE the LIMIT.
        // The old query carried pairs across LIMIT 1000, so a popular fof reached by
        // 10 friends consumed 10 slots and crowded out other fof vertices entirely.
        // avg(l.weight) over the truncated set was then biased toward fof with many friends.
        // Now LIMIT 1000 is on unique fof vertices; friend_count is pre-aggregated.
        // FOF-GO-001: replaced 3-hop MATCH with GO FROM pipe chain.
        // MATCH invokes the property-index planner on every hop even when the
        // source VID is fully known.  GO FROM uses a direct VID-indexed edge
        // scan and is 3-5× faster for known-VID traversals (Vesoft benchmark).
        // friend_count survives the third hop via the $- carry-through pattern.
        let addr_vid = vid_user(user_address);
        // Note: purchase likes (reaction_type="purchase") are excluded here via
        // WHERE != "purchase" to prevent double-counting with get_purchase_fof_recommendations.
        // Migration 15 writes purchases to both `likes` (with reaction_type="purchase")
        // and the dedicated `purchases` edge type. Without this filter, a purchase is
        // boosted once by get_fof_recommendations and again by get_purchase_fof_recommendations,
        // producing ~2× the intended purchase weight in apply_cache_boosts.
        // Long-term: backfill + delete reaction_type="purchase" rows from `likes`.
        let query = format!(
            r#"USE {space};
GO 1 STEPS FROM "{addr_vid}" OVER {e_follows}
  YIELD dst(edge) AS f_vid
  ORDER BY {e_follows}.{p_weight} DESC
  LIMIT 500
| GO 1 STEPS FROM $-.f_vid OVER {e_follows}
  YIELD $-.f_vid AS f_vid, dst(edge) AS fof_vid
  LIMIT 5000
| GROUP BY $-.fof_vid
  YIELD $-.fof_vid AS fof_vid,
        count(DISTINCT $-.f_vid) AS friend_count
| ORDER BY friend_count DESC
  LIMIT 1000
| GO 1 STEPS FROM $-.fof_vid OVER {e_likes}
  WHERE {e_likes}.{p_rt} != "purchase"
  YIELD $-.fof_vid AS fof_vid, $-.friend_count AS friend_count,
        dst(edge) AS n_vid, {e_likes}.{p_weight} AS w, {e_likes}.{p_liked_at} AS liked_at
  LIMIT 5000
| GROUP BY $-.n_vid
  YIELD $-.n_vid AS post_id,
        (count(DISTINCT $-.fof_vid) * 2.0 + sum($-.friend_count) * 1.5)
            * avg($-.w)
            * CASE WHEN max($-.liked_at) IS NULL OR max($-.liked_at) > timestamp() THEN 1.0
               ELSE exp(-1.0 * toFloat(timestamp() - max($-.liked_at)) / 86400.0 / 7.0)
               END AS score
| ORDER BY score DESC LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_follows = EDGE_FOLLOWS,
            e_likes = EDGE_LIKES,
            p_weight = PROP_WEIGHT,
            p_liked_at = PROP_LIKED_AT,
            p_rt = PROP_REACTION_TYPE,
        );

        self.fof_traverse(user_address, &query, FofBucket::FollowLike).await
    }

    /// Dwell-weighted FoF — traverses view_event edges instead of likes.
    ///
    /// Content your friends watched for a long time is a stronger signal than
    /// a like (which takes one tap). Score weights dwell time exponentially:
    /// 30s view > 10 quick likes in terms of prediction quality.
    ///
    /// 3-day exponential half-life: views are ephemeral — content you watched
    /// three days ago is no longer in active consideration. ByteGraph signal
    /// shelf life: view=3d is the shortest decay, matched to session-length browsing.
    pub async fn get_view_event_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Result<Vec<(String, f64)>> {
        let user_address = checked_address(user_address, "get_view_event_fof_recommendations")?;
        let user_address = user_address.as_str();

        // FOF-GO-002: replaced 2-hop MATCH with GO FROM pipe chain.
        // NOTE: GO WHERE on edge properties is evaluated at graphd, NOT storaged.
        // view_event_duration_idx (migration 06) is only consumed by LOOKUP ON,
        // not by GO FROM — storaged deserializes and ships all edges to graphd,
        // which then applies the WHERE filter. The filter still reduces result set
        // size for downstream stages, but does not avoid edge deserialization.
        let addr_vid = vid_user(user_address);
        let query = format!(
            r#"USE {space};
GO 1 STEPS FROM "{addr_vid}" OVER {e_follows}
  YIELD dst(edge) AS f_vid
  ORDER BY {e_follows}.{p_weight} DESC
  LIMIT 500
| GO 1 STEPS FROM $-.f_vid OVER {e_view_event}
  WHERE {e_view_event}.{p_dur} > 5
  YIELD $-.f_vid AS f_vid, dst(edge) AS n_vid,
        {e_view_event}.{p_dur} AS dur_secs,
        {e_view_event}.{p_event_time} AS etime
  LIMIT 10000
| GROUP BY $-.n_vid
  YIELD $-.n_vid AS post_id,
        count(DISTINCT $-.f_vid) AS friend_count,
        sum(toFloat($-.dur_secs)) AS total_dwell,
        max($-.etime) AS most_recent_view
| YIELD $-.post_id AS post_id,
        $-.friend_count * ($-.total_dwell / 60.0)
            * CASE WHEN $-.most_recent_view IS NULL OR $-.most_recent_view > timestamp() THEN 1.0
               ELSE exp(-1.0 * toFloat(timestamp() - $-.most_recent_view) / 86400.0 / 3.0)
               END AS score
| ORDER BY score DESC LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_follows = EDGE_FOLLOWS,
            e_view_event = EDGE_VIEW_EVENT,
            p_dur = PROP_DURATION_SECONDS,
            p_event_time = PROP_EVENT_TIME,
            p_weight = PROP_WEIGHT,
        );

        self.fof_traverse(user_address, &query, FofBucket::ViewEvent).await
    }

    /// Graph-walked user suggestions for the "Who to Follow" surface on a profile page.
    ///
    /// Strategy: people who have viewed `viewing_creator`'s content are the audience
    /// most likely to also enjoy related creators. We walk their follow edges to surface
    /// users not yet followed by `viewer_address`. Ranked by how many of the creator's
    /// audience follow them — a proxy for "well-known in this community."
    ///
    /// Two-pass approach eliminates the last remaining MATCH query:
    /// Pass 1 (GO FROM viewer OVER follows) builds a Rust HashSet of already-followed VIDs.
    /// Pass 2 (GO FROM creator OVER creator_affinity REVERSELY → follows) finds suggested users.
    /// Rust-side filter replaces the MATCH WITH-collect anti-join, which was 3-5× slower
    /// than VID-indexed GO FROM traversals (Vesoft benchmark).
    pub async fn get_viewer_based_user_suggestions(
        &self,
        viewer_address: &str,
        viewing_creator: &str,
        limit: usize,
    ) -> Result<Vec<(String, f64)>> {
        let viewer_address  = checked_address(viewer_address,  "get_viewer_based_user_suggestions")?;
        let viewing_creator = checked_address(viewing_creator, "get_viewer_based_user_suggestions")?;
        let viewer_address  = viewer_address.as_str();
        let viewing_creator = viewing_creator.as_str();
        let safe_limit = limit.min(50);

        if let Some(ref cache) = self.cache {
            if let Some(cached) = cache.get_user_suggestions(viewer_address, viewing_creator).await {
                return Ok(cached.into_iter().map(|(k, v)| (k, v as f64)).collect());
            }
        }

        let viewer_vid = vid_user(viewer_address);
        let creator_vid = vid_user(viewing_creator);

        // Pass 1: collect the set of VIDs the viewer already follows.
        // Direct VID-indexed scan — no index planner overhead.
        let query1 = format!(
            r#"USE {space};
GO 1 STEPS FROM "{viewer_vid}" OVER {e_follows}
  YIELD dst(edge) AS fid
  LIMIT 2000;"#,
            space = SPACE_THERAGRAPH,
            viewer_vid = viewer_vid,
            e_follows = EDGE_FOLLOWS,
        );
        // NEBULA-005: get_viewer_based_user_suggestions must use execute_query_uncached
        // so each call reflects the current follow graph — a cached stale result would
        // filter the wrong "already following" set and surface users the viewer just
        // followed. fof_traverse already uses execute_query_uncached; align here.
        let output1 = self.execute_query_uncached(&query1).await?;
        let already_following: HashSet<String> = output1
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.starts_with('+') || line.is_empty() {
                    return None;
                }
                let mut it = line.split('|').map(str::trim);
                let _ = it.next();
                let Some(cell) = it.next() else { return None; };
                let vid = cell.trim_matches('"');
                if vid.is_empty() || vid.contains(' ') {
                    None
                } else {
                    Some(vid.to_string())
                }
            })
            .collect();

        // Pass 2: walk from the creator via creator_affinity REVERSELY to find viewers
        // of their content, then follow those viewers' follows edges to surface suggested
        // users. REVERSELY uses storaged's incoming-edge scan — no additional index required.
        // Fetch safe_limit*4 candidates so the Rust filter has enough to fill safe_limit slots.
        let query2 = format!(
            r#"USE {space};
GO 1 STEPS FROM "{creator_vid}" OVER {e_creator_affinity} REVERSELY
  YIELD src(edge) AS viewer_vid
  LIMIT 300
| GO 1 STEPS FROM $-.viewer_vid OVER {e_follows}
  YIELD $-.viewer_vid AS src_vid, dst(edge) AS suggested_vid
  LIMIT 10000
| GROUP BY $-.suggested_vid
  YIELD $-.suggested_vid AS user_id,
        toFloat(count(DISTINCT $-.src_vid)) AS mutual_count
| ORDER BY mutual_count DESC LIMIT {over_limit};"#,
            space = SPACE_THERAGRAPH,
            creator_vid = creator_vid,
            e_creator_affinity = EDGE_CREATOR_AFFINITY,
            e_follows = EDGE_FOLLOWS,
            over_limit = safe_limit * 4,
        );
        let output2 = self.execute_query_uncached(&query2).await?;
        let raw = parse_nebula_table(&output2, 1, 2);

        // Rust-side anti-join: remove already-followed users, the viewer themselves,
        // and the creator whose profile the viewer is already on (suggesting "follow
        // this person" while you're browsing their profile is redundant and confusing).
        let creator_vid = vid_user(viewing_creator);
        let results: Vec<(String, f64)> = raw
            .into_iter()
            .filter(|(id, _)| {
                id.as_str() != viewer_vid.as_str()
                    && id.as_str() != creator_vid.as_str()
                    && !already_following.contains(id.as_str())
            })
            .take(safe_limit)
            .collect();

        if let Some(ref cache) = self.cache {
            cache.set_user_suggestions(viewer_address, viewing_creator, &results).await;
        }

        Ok(results)
    }

    /// Parameterised scaffold shared by comment/purchase/share/bookmark FoF methods.
    ///
    /// All four follow the same pipe structure:
    ///   follows LIMIT 200 → signal_edge LIMIT 5000 → GROUP BY → score → ORDER LIMIT 50
    ///
    /// Score formula is determined by `FofSimpleScore`:
    ///   Weighted(m)    → friend_count × m × exp(-age/hl)
    ///   CountWeighted  → interaction_count × friend_count × exp(-age/hl)
    async fn fof_traverse_simple(
        &self,
        user_address: &str,
        cfg: FofSimpleConfig,
    ) -> Result<Vec<(String, f64)>> {
        let addr = checked_address(user_address, cfg.fn_name)?;
        let addr = addr.as_str();
        let addr_vid = vid_user(addr);

        // Deferred binding holds the heap-allocated String for the Weighted branch;
        // CountWeighted borrows static consts — zero intermediate allocation.
        let weighted_score_buf;
        let (extra_agg, score_expr): (&str, &str) = match cfg.score {
            FofSimpleScore::Weighted(m) => {
                weighted_score_buf = format!("toFloat($-.friend_count) * {m}");
                ("", &weighted_score_buf)
            }
            FofSimpleScore::CountWeighted => (COUNT_EXTRA_AGG, COUNT_SCORE_EXPR),
        };

        let query = format!(
            r#"USE {space};
GO 1 STEPS FROM "{addr_vid}" OVER {e_follows}
  YIELD dst(edge) AS f_vid
  LIMIT 200
| GO 1 STEPS FROM $-.f_vid OVER {edge_type}
  YIELD $-.f_vid AS f_vid, dst(edge) AS n_vid,
        {edge_type}.{ts_prop} AS {ts_alias}
  LIMIT 5000
| GROUP BY $-.n_vid
  YIELD $-.n_vid AS post_id,
        count(DISTINCT $-.f_vid) AS friend_count,
{extra_agg}        max($-.{ts_alias}) AS most_recent
| YIELD $-.post_id AS post_id,
        {score_expr}
            * CASE WHEN $-.most_recent IS NULL OR $-.most_recent > timestamp() THEN 1.0
               ELSE exp(-1.0 * toFloat(timestamp() - $-.most_recent) / 86400.0 / {hl})
               END AS score
| ORDER BY score DESC LIMIT 50;"#,
            space     = SPACE_THERAGRAPH,
            e_follows = EDGE_FOLLOWS,
            edge_type = cfg.edge_type,
            ts_prop   = cfg.ts_prop,
            ts_alias  = cfg.ts_alias,
            hl        = cfg.half_life,
        );

        self.fof_traverse(addr, &query, cfg.bucket).await
    }

    /// Comment-weighted FoF. Score = comment_count × friend_count × exp(-age/14d).
    ///
    /// 14-day half-life: composing text is deliberate engagement — decays slower than
    /// a passive view (3d) or single-tap like (7d).
    /// MIGRATION note: FOF-GO-003 replaced 2-hop MATCH with GO FROM pipe — ~1.2 MB
    /// fewer intermediate bytes per query at the 5000-edge limit.
    pub async fn get_comment_fof_recommendations(&self, user_address: &str) -> Result<Vec<(String, f64)>> {
        self.fof_traverse_simple(user_address, FofSimpleConfig {
            fn_name:   "get_comment_fof_recommendations",
            edge_type: EDGE_COMMENTS_ON,
            ts_prop:   PROP_COMMENTED_AT,
            ts_alias:  "commented_at",
            half_life: 14.0,
            score:     FofSimpleScore::CountWeighted,
            bucket:    FofBucket::Comment,
        }).await
    }

    /// Purchase-weighted FoF. Score = friend_count × 3.0 × exp(-age/30d).
    ///
    /// 30-day half-life: ownership persists. 3× multiplier: a friend purchase
    /// outweighs a like 3× in prediction quality.
    /// MIGRATION 15: uses dedicated `purchases` edge — no WHERE clause over likes.
    /// BACKFILL: pre-migration purchases in `likes` (reaction_type="purchase") are
    /// not returned here; run theragraph-nebula/init/15-add-purchases-edge.ngql.
    pub async fn get_purchase_fof_recommendations(&self, user_address: &str) -> Result<Vec<(String, f64)>> {
        self.fof_traverse_simple(user_address, FofSimpleConfig {
            fn_name:   "get_purchase_fof_recommendations",
            edge_type: EDGE_PURCHASES,
            ts_prop:   PROP_PURCHASED_AT,
            ts_alias:  "purchased_at",
            half_life: 30.0,
            score:     FofSimpleScore::Weighted(3.0),
            bucket:    FofBucket::Purchase,
        }).await
    }

    /// Share-weighted FoF. Score = friend_count × 1.5 × exp(-age/15d).
    ///
    /// 15d half-life: shares persist in social context longer than a like but
    /// lose relevance faster than economic commitment (purchase 30d).
    pub async fn get_shared_fof_recommendations(&self, user_address: &str) -> Result<Vec<(String, f64)>> {
        self.fof_traverse_simple(user_address, FofSimpleConfig {
            fn_name:   "get_shared_fof_recommendations",
            edge_type: EDGE_SHARED,
            ts_prop:   PROP_SHARED_AT,
            ts_alias:  "shared_at",
            half_life: 15.0,
            score:     FofSimpleScore::Weighted(1.5),
            bucket:    FofBucket::Share,
        }).await
    }

    /// Bookmark-weighted FoF. Score = friend_count × 1.2 × exp(-age/10d).
    ///
    /// 10d half-life: saved content loses relevance faster than shares as the
    /// feed evolves. 1.2× above view/comment, below share (1.5×).
    pub async fn get_bookmark_fof_recommendations(&self, user_address: &str) -> Result<Vec<(String, f64)>> {
        self.fof_traverse_simple(user_address, FofSimpleConfig {
            fn_name:   "get_bookmark_fof_recommendations",
            edge_type: EDGE_BOOKMARKED,
            ts_prop:   PROP_BOOKMARKED_AT,
            ts_alias:  "bookmarked_at",
            half_life: 10.0,
            score:     FofSimpleScore::Weighted(1.2),
            bucket:    FofBucket::Bookmark,
        }).await
    }

    /// Flix-watch FoF. Score = friend_count × 2.0 × exp(-age/14d).
    ///
    /// 14-day half-life: video completion is deliberate intent — decays slower than
    /// a like (7d) but faster than a purchase (30d). 2.0× multiplier: a friend
    /// who watched a flix is a stronger signal than a view_event (1.0×) because
    /// `flix_watch` only fires on intentional playback, not feed scroll-past.
    /// pct_played + rewatch_count (stored on the edge) are not yet folded into the
    /// FoF score here — they weight the edge in future traversal passes.
    pub async fn get_flix_watch_fof_recommendations(&self, user_address: &str) -> Result<Vec<(String, f64)>> {
        self.fof_traverse_simple(user_address, FofSimpleConfig {
            fn_name:   "get_flix_watch_fof_recommendations",
            edge_type: EDGE_FLIX_WATCH,
            ts_prop:   PROP_WATCHED_AT,
            ts_alias:  "watched_at",
            half_life: 14.0,
            score:     FofSimpleScore::Weighted(2.0),
            bucket:    FofBucket::FlixWatch,
        }).await
    }

    /// Read style_preference edges written by `write_companion_preference_edges`
    /// (AiFriendz chat → music/flix companion signal). Returns tag (with the
    /// `style:` VID prefix stripped) → confidence (0.0–1.0), highest confidence
    /// first, capped at 50.
    ///
    /// Cached in Redis for `STYLE_PREFS_TTL` — unlike FoF (pre-warmed by a
    /// background job), this traversal is only ever invoked from the
    /// synchronous feed request path (`apply_cache_boosts`), so a request-path
    /// cache keeps latency bounded between the infrequent chat-driven writes.
    pub async fn get_style_preferences(&self, user_address: &str) -> Result<HashMap<Box<str>, f32>> {
        let user_address = checked_address(user_address, "get_style_preferences")?;
        let user_address = user_address.as_str();

        if let Some(ref cache) = self.cache {
            if let Some(cached) = cache.get_style_preferences(user_address).await {
                return Ok(cached);
            }
        }

        let addr_vid = vid_user(user_address);
        let query = format!(
            r#"USE {space};
GO FROM "{addr_vid}" OVER {e_style}
  YIELD dst(edge) AS tag_vid, {e_style}.{p_conf} AS conf
| ORDER BY conf DESC
  LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_style = EDGE_STYLE_PREFERENCE,
            p_conf = PROP_CONFIDENCE,
        );

        let output = self.execute_query_uncached(&query).await?;
        let pairs = parse_nebula_table(&output, 1, 2);

        let result: HashMap<Box<str>, f32> = pairs
            .into_iter()
            .filter_map(|(vid, conf)| {
                vid.strip_prefix("style:").map(|tag| (Box::<str>::from(tag), conf as f32))
            })
            .collect();

        if let Some(ref cache) = self.cache {
            cache.set_style_preferences(user_address, &result).await;
        }

        Ok(result)
    }

    /// Read `genre_preference` edges — a user's declared favorite genres,
    /// written via `write_genre_preference_edges` (up to 30 slugs; see
    /// `POST /api/v1/genre-preferences/{addr}`) or the companion-signal path
    /// in `write_companion_preference_edges`. Returns genre slug (with the
    /// `genre:` VID prefix stripped) → confidence (0.0–1.0), highest
    /// confidence first, capped at 50.
    ///
    /// GENRE-01: previously unreadable — the old numeric `genre:{id}` VID
    /// format had no id→name mapping reachable from this crate (see the
    /// removed comment on `get_style_preferences`). Now that NFT-side genre
    /// (folded into `tags` at ingestion — `event_processor::elixir_db::
    /// process_enrichment`) and listener-side genre preference both live in
    /// the same kebab-case slug namespace, matching them is a direct
    /// tag-string comparison — exactly how `apply_cache_boosts` already
    /// matches `style_preference` tags against `ScoredNft::tags`.
    ///
    /// Cached in Redis for `GENRE_PREFS_TTL`, same request-path cache-aside
    /// shape as `get_style_preferences` — no background pre-warm job (that is
    /// deliberately a separate, later pass).
    pub async fn get_genre_preferences(&self, user_address: &str) -> Result<HashMap<Box<str>, f32>> {
        let user_address = checked_address(user_address, "get_genre_preferences")?;
        let user_address = user_address.as_str();

        if let Some(ref cache) = self.cache {
            if let Some(cached) = cache.get_genre_preferences(user_address).await {
                return Ok(cached);
            }
        }

        let addr_vid = vid_user(user_address);
        let query = format!(
            r#"USE {space};
GO FROM "{addr_vid}" OVER {e_genre}
  YIELD dst(edge) AS genre_vid, {e_genre}.{p_conf} AS conf
| ORDER BY conf DESC
  LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_genre = EDGE_GENRE_PREFERENCE,
            p_conf = PROP_CONFIDENCE,
        );

        let output = self.execute_query_uncached(&query).await?;
        let pairs = parse_nebula_table(&output, 1, 2);

        let result: HashMap<Box<str>, f32> = pairs
            .into_iter()
            .filter_map(|(vid, conf)| {
                vid.strip_prefix("genre:").map(|slug| (Box::<str>::from(slug), conf as f32))
            })
            .collect();

        if let Some(ref cache) = self.cache {
            cache.set_genre_preferences(user_address, &result).await;
        }

        Ok(result)
    }

    /// Batch-write `recommended_to` edges for a set of served NFTs.
    ///
    /// Called after the engine computes and returns a feed. Each edge records
    /// the score at serve-time and `served = false` (unclicked). When the user
    /// clicks/purchases, `mark_recommendation_served` flips `served = true`.
    ///
    /// Fires in a background task — must not block the API response path.
    /// Best-effort: errors are logged but never propagated.
    ///
    /// For flushing an entire batch of users in one Nebula round-trip, prefer
    /// `write_recommended_to_batch_multi` (called by the buffer flusher).
    pub async fn write_recommended_to_batch(
        &self,
        user_address: &str,
        served: &[(Box<str>, f32)],
    ) -> Result<()> {
        let body = match build_recommended_to_body(user_address, served) {
            Some(b) => b,
            None => return Ok(()),
        };
        let mut query = String::with_capacity(32 + body.len());
        let _ = write!(query, "USE {SPACE_THERAGRAPH};\n");
        query.push_str(&body);
        // S30-15: propagate error so caller can log at the call site.
        self.execute_write(&query).await
            .map(|_| ())
            .map_err(|e| {
                error!("Nebula write_recommended_to_batch failed ({} items): {}", served.len(), e);
                e
            })
    }

    /// Write `recommended_to` edges for all users in a flush batch using a single
    /// Nebula round-trip.
    ///
    /// `build_recommended_to_body` is called per user (min-max score normalisation
    /// is per-user), bodies are concatenated after one shared `USE` preamble, and
    /// `execute_write` fires once. At FLUSH_BATCH_SIZE=100 users, this reduces
    /// Nebula connection count from 100 to 1 per 500ms flush interval.
    pub async fn write_recommended_to_batch_multi(
        &self,
        batch: &[(String, Box<[(Box<str>, f32)]>)],
    ) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let total_nfts: usize = batch.iter().map(|(_, p)| p.len()).sum();
        // One USE header (32 bytes) + per-user estimates (130 user vertex + 540/NFT).
        let est_cap = 32 + batch.len() * 130 + total_nfts * 540;
        let mut query = String::with_capacity(est_cap);
        let _ = write!(query, "USE {SPACE_THERAGRAPH};\n");
        let mut any_valid = false;
        for (addr, pairs) in batch {
            if let Some(body) = build_recommended_to_body(addr, pairs) {
                query.push_str(&body);
                any_valid = true;
            }
        }
        if !any_valid {
            return Ok(());
        }
        self.execute_write(&query).await
            .map(|_| ())
            .map_err(|e| {
                error!(
                    "Nebula write_recommended_to_batch_multi failed ({} users, {} items): {}",
                    batch.len(), total_nfts, e
                );
                e
            })
    }

    /// Mark a `recommended_to` edge as clicked/purchased (served = true).
    ///
    /// Called by the interaction API when the user opens or purchases an NFT
    /// that was served via the recommendation engine. This closes the feedback
    /// loop: the engine now knows its recommendation was acted upon.
    pub async fn mark_recommendation_served(&self, user_address: &str, nft_id: &str) {
        if !is_safe_address(user_address) || !is_safe_post_vid_id(nft_id) {
            warn!("mark_recommendation_served: invalid input — addr={user_address} nft={nft_id}");
            return;
        }
        // VID-CASE-001: normalise to lowercase.
        let user_address = normalize_address(user_address);
        // P3-02: UPDATE EDGE (not UPSERT EDGE) — if the edge doesn't exist (no prior
        // write_recommended_to_batch call for this pair), UPDATE is a no-op.
        // UPSERT would create a phantom served=true edge with score=0 and no computed_at,
        // poisoning the feedback-loop query that reads served=true edges.
        let addr_vid = vid_user(&user_address);
        let nft_vid = vid_post(nft_id);
        let query = format!(
            r#"USE {space};
UPDATE EDGE ON {e_rec_to} "{addr_vid}" -> "{nft_vid}"@0
SET {p_served} = true,
    {p_computed_at} = now()
WHEN {e_rec_to}.{p_served} == false
YIELD {e_rec_to}.{p_served} AS served;"#,
            space = SPACE_THERAGRAPH,
            e_rec_to = EDGE_RECOMMENDED_TO,
            addr_vid = addr_vid,
            nft_vid = nft_vid,
            p_served = PROP_SERVED,
            p_computed_at = PROP_COMPUTED_AT,
        );
        if let Err(e) = self.execute_write(&query).await {
            error!("Nebula mark_recommendation_served failed (best-effort): {e}");
        }
    }

    /// Batch-flip `served = true` on multiple `recommended_to` edges in a single nGQL call.
    ///
    /// Replaces the N-subprocess-per-request loop in the API handler (finding 2 from S24
    /// audit). Same UPDATE EDGE ON semantics as the single variant — no-op when edge doesn't
    /// exist, never creates phantom edges.
    pub async fn mark_recommendations_served_batch(&self, user_address: &str, nft_ids: &[String]) {
        if nft_ids.is_empty() {
            return;
        }
        if !is_safe_address(user_address) {
            warn!("mark_recommendations_served_batch: invalid address={user_address}");
            return;
        }
        let user_address = normalize_address(user_address);
        let addr_vid = vid_user(&user_address);
        let mut query = format!("USE {SPACE_THERAGRAPH};\n");
        query.reserve(nft_ids.len() * 250);
        let mut has_valid = false;
        for nft_id in nft_ids {
            if !is_safe_post_vid_id(nft_id) {
                warn!("mark_recommendations_served_batch: unsafe nft_id={nft_id} — skipping");
                continue;
            }
            let nft_vid = vid_post(nft_id);
            let _ = write!(
                query,
                "UPDATE EDGE ON {e_rec_to} \"{addr_vid}\" -> \"{nft_vid}\"@0 \
                 SET {p_served} = true, {p_computed_at} = now() \
                 WHEN {e_rec_to}.{p_served} == false \
                 YIELD {e_rec_to}.{p_served} AS served;\n",
                e_rec_to = EDGE_RECOMMENDED_TO,
                addr_vid = addr_vid,
                nft_vid = nft_vid,
                p_served = PROP_SERVED,
                p_computed_at = PROP_COMPUTED_AT,
            );
            has_valid = true;
        }
        if !has_valid {
            return;
        }
        if let Err(e) = self.execute_write(&query).await {
            error!("Nebula mark_recommendations_served_batch failed ({} items): {e}", nft_ids.len());
        }
    }
}
