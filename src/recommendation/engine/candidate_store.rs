//! `CandidateStore` — paired access handle for the PgPool + optional RecCache.
//!
//! Every candidate query needs both. Bundling them in one cloneable struct lets
//! the coalescer capture one value instead of two and names the concept at the seam.

use sqlx::PgPool;
use crate::recommendation::cache::RecCache;

#[derive(Clone)]
pub(super) struct CandidateStore {
    pub(super) pool:  PgPool,
    pub(super) cache: Option<RecCache>,
}

impl CandidateStore {
    pub(super) fn new(pool: PgPool) -> Self {
        Self { pool, cache: None }
    }

    pub(super) fn with_cache(mut self, cache: Option<RecCache>) -> Self {
        self.cache = cache;
        self
    }

    pub(super) fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(super) fn cache_ref(&self) -> Option<&RecCache> {
        self.cache.as_ref()
    }
}
