//! Dirty-plus-debounce invalidation of a user's cached recommendations.
//!
//! Before: every positive interaction (a like, and every card view) deleted the person's cached preferences and
//! recommendation results in Redis AND the SQL `recommendation_cache` row, so a burst of ten likes forced ten
//! recomputes (each ~5 to 7 SQL plus a batched Nebula write). Now an interaction only MARKS the cache dirty
//! (`rec:dirty:v1:<addr>`, one SET NX with a safety TTL: the first event of a burst wins), and the next feed read
//! (`try_get_cached_free`) purges both tiers and recomputes only if the mark is at least the debounce old. A burst
//! therefore causes ONE recompute, and a cached feed can be at most the debounce staler than before (it was already
//! allowed to live `RECOMMENDATION_TTL`, 10 minutes). The person's own like still shows instantly in the UI (that is
//! client state); the session-recency signal is still appended immediately, so what they just did still counts.
//!
//! `REC_DIRTY_DEBOUNCE_SECS` (default 60, clamped to 0..=3600) tunes it; `0` restores the old delete-on-every-event.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use redis::AsyncCommands;
use tracing::warn;

use super::keys::{CacheKey, REC_DIRTY_TTL};
use super::ops::RecCache;

/// Seconds a dirty mark must age before a read recomputes (a conservative default).
pub(crate) const DEFAULT_REC_DIRTY_DEBOUNCE_SECS: i64 = 60;
const MAX_REC_DIRTY_DEBOUNCE_SECS: i64 = 3600;

/// The configured debounce, read once. `0` means "delete on every event", the pre-debounce behaviour.
pub fn rec_dirty_debounce_secs() -> i64 {
    static DEBOUNCE: OnceLock<i64> = OnceLock::new();
    *DEBOUNCE.get_or_init(|| parse_debounce(std::env::var("REC_DIRTY_DEBOUNCE_SECS").ok().as_deref()))
}

/// Pure parse of the setting: a missing or unreadable value is the default; the range is 0..=3600.
pub(crate) fn parse_debounce(raw: Option<&str>) -> i64 {
    match raw.map(str::trim).filter(|s| !s.is_empty()).map(str::parse::<i64>) {
        Some(Ok(v)) => v.clamp(0, MAX_REC_DIRTY_DEBOUNCE_SECS),
        _ => DEFAULT_REC_DIRTY_DEBOUNCE_SECS,
    }
}

/// Is a mark written at `marked_at` (unix secs, as stored) at least `debounce` seconds old at `now`?
/// An unreadable mark counts as due: when in doubt, prefer a fresh feed over a stale one.
pub(crate) fn is_due(marked_at: &str, now: i64, debounce: i64) -> bool {
    match marked_at.trim().parse::<i64>() {
        Ok(at) => now.saturating_sub(at) >= debounce,
        Err(_) => true,
    }
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

impl RecCache {
    /// Mark the user's cached recommendations dirty. `SET NX EX`: the FIRST event of a burst sets the time, later
    /// events leave it (one cheap round trip, no delete). Best-effort: an error is logged and the cache simply
    /// expires on its own TTL.
    pub async fn mark_recs_dirty(&self, user_address: &str) {
        let key = CacheKey::rec_dirty(user_address);
        let mut conn = self.conn_clone();
        let res: redis::RedisResult<Option<String>> = redis::cmd("SET")
            .arg(&key)
            .arg(unix_now())
            .arg("NX")
            .arg("EX")
            .arg(REC_DIRTY_TTL)
            .query_async(&mut conn)
            .await;
        if let Err(e) = res {
            warn!("RecCache SET NX dirty mark error for {}: {}", user_address, e);
        }
    }

    /// If the user's mark is at least `debounce_secs` old, remove it and return `true`: the caller must now purge
    /// the cached recommendations and recompute. No mark, or a mark still inside the debounce, returns `false`
    /// (the cached feed is served). Two readers racing both see "due": both purge, which is harmless.
    pub async fn take_recs_dirty_if_due(&self, user_address: &str, debounce_secs: i64) -> bool {
        let key = CacheKey::rec_dirty(user_address);
        let mut conn = self.conn_clone();
        let raw: Option<String> = match conn.get(&key).await {
            Ok(v) => v,
            Err(e) => {
                warn!("RecCache GET dirty mark error for {}: {}", user_address, e);
                return false;
            }
        };
        match raw {
            Some(marked_at) if is_due(&marked_at, unix_now(), debounce_secs) => {
                self.del_key(&key).await;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_defaults_to_sixty_and_is_clamped() {
        assert_eq!(parse_debounce(None), 60);
        assert_eq!(parse_debounce(Some("")), 60);
        assert_eq!(parse_debounce(Some("soon")), 60);
        assert_eq!(parse_debounce(Some("15")), 15);
        assert_eq!(parse_debounce(Some(" 120 ")), 120);
        assert_eq!(parse_debounce(Some("0")), 0, "0 is the legacy delete-on-every-event switch");
        assert_eq!(parse_debounce(Some("-5")), 0);
        assert_eq!(parse_debounce(Some("999999")), 3600);
    }

    #[test]
    fn a_mark_is_due_only_once_it_is_old_enough() {
        assert!(!is_due("1000", 1059, 60));
        assert!(is_due("1000", 1060, 60));
        assert!(is_due("1000", 5000, 60));
        assert!(is_due("not a time", 1000, 60), "an unreadable mark errs toward a fresh feed");
        assert!(!is_due("1000", 999, 60), "a mark from the future is not due");
    }

    #[test]
    fn the_dirty_key_is_lowercased_and_versioned() {
        assert_eq!(CacheKey::rec_dirty("0xABC"), "rec:dirty:v1:0xabc");
    }

    // ── Live Redis (runs only with REDIS_URL, like the other cache tests) ─────────────────────────────────────

    async fn live_cache() -> Option<RecCache> {
        let url = std::env::var("REDIS_URL").ok()?;
        RecCache::connect(&url).await
    }

    #[tokio::test]
    async fn a_burst_ages_one_mark_and_a_due_read_takes_it_once() {
        let Some(cache) = live_cache().await else { return };
        let addr = format!("0xdirtytest{}", unix_now());
        let key = CacheKey::rec_dirty(&addr);
        cache.del_key(&key).await;

        cache.mark_recs_dirty(&addr).await;
        let first = cache.get_raw(&key).await.expect("marked");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        cache.mark_recs_dirty(&addr).await; // later events of the burst: NX keeps the FIRST time
        assert_eq!(cache.get_raw(&key).await.as_deref(), Some(first.as_str()));

        assert!(!cache.take_recs_dirty_if_due(&addr, 3600).await, "inside the debounce the cached feed is served");
        assert!(cache.get_raw(&key).await.is_some(), "and the mark stays");
        assert!(cache.take_recs_dirty_if_due(&addr, 0).await, "once due, the read takes it");
        assert!(!cache.take_recs_dirty_if_due(&addr, 0).await, "taken once: one recompute per burst");
        assert!(cache.get_raw(&key).await.is_none());
    }
}

