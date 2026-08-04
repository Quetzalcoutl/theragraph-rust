//! User Preferences and Interaction Tracking
//!
//! Re-export facade — all public types and functions are available here for
//! backward compatibility.  Internals are split into two sub-modules:
//!
//! - [`super::model`] — pure domain types, constants, and functions (no I/O)
//! - [`super::recorder`] — async database and Redis cache I/O
//!
//! Callers that import from `preferences` require no changes.

pub use super::model::*;
pub use super::recorder::*;
