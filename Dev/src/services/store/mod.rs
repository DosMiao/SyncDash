//! L1 on-disk state that outlives a single run.
//!
//! `settings` is app-level configuration, `trash` the local recycle store and its retention,
//! `version` the per-root version history, `hashcache` and `mtimefix` the two per-root tables the
//! scanner and the applier hand each other. All of them are best-effort: failing to record
//! something must never fail the sync itself.

pub(crate) mod atomic;
pub mod hashcache;
pub(crate) mod localid;
pub mod mtimefix;
pub(crate) mod scan_state;
pub mod settings;
pub mod trash;
pub mod version;
pub mod watch;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanCoverage {
    /// The walker completed without errors. State for absent paths in the scanner-provided domain
    /// may be removed; deliberately filtered paths can still be retained individually.
    Complete,
    /// The walker skipped entries because of errors, so absence proves nothing for this scan.
    /// New observations still update state and invalidate state that they directly contradict.
    Partial,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateWriteStatus {
    Written,
    Unchanged,
    PreservedNewerVersion,
    Failed,
}
