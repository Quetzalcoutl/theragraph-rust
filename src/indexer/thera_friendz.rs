// Thin compatibility shim for the new TheraFriendz naming
// Re-exports the implementation from the legacy `thera_social` module so
// callers can use either `indexer::thera_friendz` or `indexer::thera_social`.

pub use crate::indexer::thera_social::*;
