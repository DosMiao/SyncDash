//! apply: execute a plan — the last of the three stages, and the only one that writes.
//!
//! Everything here is arranged around one rule: an interruption at any moment must leave the
//! target either as it was or as the plan intended, never half way. Writes stage into a temp name
//! and land by rename, both roots are locked for the duration, and what is overwritten or deleted
//! is preserved first.
//!
//! Ops run in five classes, deepest-first for directory removal, because the order the plan
//! arrives in is not trusted — see `execute::schedule`.
//!
mod coordination;
mod execute;
mod lease;
mod reporting;
mod validation;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use crate::model::plan::Op;
use crate::obs::progress::{ApplyOutcome, RunCtx};

pub struct ApplyOptions {
    pub dry_run: bool,
    pub trash: Option<PathBuf>,
    pub verbose: bool,
    /// paranoid rigor tier: after a copy/update, re-read the destination and verify blake3 (same as FFS "verify copied files")
    pub verify: bool,
    /// Versioning (optional): deleted/overwritten files go into each root's .version_syncDash/ instead of the local trash
    pub versioning: bool,
    /// Whether to fsync the temp file before the rename. Default true; can be turned off if it feels slow over SMB (at your own risk)
    pub fsync: bool,
    /// Used on directory deletion to decide "may the leftovers inside be removed along with it" (syncthing's `(?d)`)
    pub filter: Option<crate::pipeline::filter::PathFilter>,
    /// Enable bounded in-memory delta staging for local or mounted regular-file updates.
    pub delta: bool,
    /// Parallel width of the Copy/Update phase (1 = sequential). Default 4: over SMB, 2-4 streams basically
    /// saturate the uplink, and going wider only worsens queueing on the far end. Clamped to 1..=16.
    pub parallel: usize,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        ApplyOptions {
            dry_run: true,
            trash: None,
            verbose: false,
            verify: false,
            versioning: false,
            fsync: true,
            filter: None,
            delta: false,
            parallel: 4,
        }
    }
}

pub fn apply(
    ops: &[Op],
    source_root: &Path,
    target_root: &Path,
    opt: &ApplyOptions,
) -> (u64, u64, u64) {
    apply_with(ops, source_root, target_root, opt, &RunCtx::null()).into_tuple()
}

/// Apply to two local paths through the same VFS transaction engine used by every backend.
pub fn apply_with(
    ops: &[Op],
    source_root: &Path,
    target_root: &Path,
    opt: &ApplyOptions,
    ctx: &RunCtx,
) -> ApplyOutcome {
    let sv: std::sync::Arc<dyn crate::fs::vfs::Vfs> =
        match crate::fs::vfs::local::LocalVfs::open(source_root.to_path_buf()) {
            Ok(root) => std::sync::Arc::new(root),
            Err(error) => {
                crate::log_error!("apply", "cannot open source root: {error}");
                return ApplyOutcome {
                    done: 0,
                    skipped: ops.len() as u64,
                    errors: 1,
                    bytes_copied: 0,
                    cancelled: false,
                };
            }
        };
    let tv: std::sync::Arc<dyn crate::fs::vfs::Vfs> =
        match crate::fs::vfs::local::LocalVfs::open(target_root.to_path_buf()) {
            Ok(root) => std::sync::Arc::new(root),
            Err(error) => {
                crate::log_error!("apply", "cannot open target root: {error}");
                return ApplyOutcome {
                    done: 0,
                    skipped: ops.len() as u64,
                    errors: 1,
                    bytes_copied: 0,
                    cancelled: false,
                };
            }
        };
    apply_vfs(ops, &sv, &tv, opt, ctx)
}

/// The Copy/Update lane's parallel width.
///
/// `pref` is what the job asked for, but a copy holds a stream open on **both** roots at once, so
/// the narrower backend governs. This is not tuning: for a backend that means its limit, exceeding
/// it is not slower, it is an error. FTP has one control connection, so a second concurrent
/// transfer fails outright with "Data connection is already open" and that file never lands — a
/// four-wide default silently reduced every `ftp://` run to one file copied and the rest errored.
///
/// The `Vfs` trait has always documented this clamp ("pool width, scan hash width, apply copy width
/// all clamp to it"); the scan lane honoured it and this one did not.
pub fn copy_width(
    pref: usize,
    src: &crate::fs::vfs::VfsCaps,
    tgt: &crate::fs::vfs::VfsCaps,
    has_dup: bool,
) -> usize {
    if has_dup {
        return 1;
    }
    pref.min(src.max_parallel_streams.min(tgt.max_parallel_streams))
        .max(1)
}

/// Execute the validated mutation schedule over any backend pair.
pub fn apply_vfs(
    ops: &[Op],
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    options: &ApplyOptions,
    context: &RunCtx,
) -> ApplyOutcome {
    coordination::execute(ops, source, target, options, context)
}
