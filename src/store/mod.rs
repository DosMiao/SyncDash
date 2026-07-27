//! L1 on-disk state that outlives a single run.
//!
//! `settings` is app-level configuration, `trash` the local recycle store and its
//! retention, `version` the per-root version history. All three are best-effort:
//! failing to record something must never fail the sync itself.

pub mod settings;
pub mod trash;
pub mod version;
