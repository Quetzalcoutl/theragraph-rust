// Thin compatibility shim for the new TheraFriends naming
// Re-exports the implementation from the legacy `thera_social` module so
// callers can use either `indexer::thera_friends` or `indexer::thera_social`.

pub use crate::indexer::thera_social::*;
