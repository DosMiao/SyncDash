//! The worker pool and the shared state one apply run hands to its threads.
//!
//! Ops are executed in classes rather than in plan order: moves before copies before deletes,
//! and directory removals deepest-first. `compare` already emits them ranked, but the input to
//! `apply` is not trusted to have come from `compare` — a plan can arrive from a package built
//! by another machine, so the ordering is re-derived here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::model::plan::Op;
use crate::obs::progress::{PhaseProgress, RunCtx};
use super::ApplyOptions;
use super::ops::exec_op;
use crate::model::plan::Side;
use super::ledger::record;

/// Execution environment shared by the worker threads. All counters are atomic and writers sit behind locks — a worker only borrows &Shared.
pub(super) struct Shared<'a> {
    pub(super) opt: &'a ApplyOptions,
    pub(super) ctx: &'a RunCtx,
    pub(super) source: &'a std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    pub(super) target: &'a std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    /// The local escape hatches, precomputed: Some = this side is a real directory and
    /// the path-based machinery (delta, VersionWriter, walkdir) applies.
    pub(super) source_local: Option<PathBuf>,
    pub(super) target_local: Option<PathBuf>,
    /// Whether the central trash store may take each side's deletions.
    ///
    /// A separate question from `local_of`, and the reason this field exists: `\\nas\share` is a
    /// real local path, so the delta lane does apply to it — but it is on another machine, so a
    /// move into the store on this one copies every deleted file across the network first.
    pub(super) source_trash_ok: bool,
    pub(super) target_trash_ok: bool,
    pub(super) trash: PathBuf,
    /// The in-root retention area for remote sides: `.syncdash/trash/<run_ms>` under
    /// the executing root — originals move there by RENAME on the far side, nothing
    /// is downloaded. Named in the preflight report before it is ever used.
    pub(super) remote_keep_rel: String,
    pub(super) ver_source: Mutex<Option<crate::store::version::VersionWriter>>,
    pub(super) ver_target: Mutex<Option<crate::store::version::VersionWriter>>,
    /// Directories already ensured on each side this run (spares one round-trip per file on remote roots)
    pub(super) mkdir_memo: Mutex<std::collections::HashSet<(bool, String)>>,
    // P1-4: when the mtime the filesystem actually stored differs from the one we wanted (FAT's 2-second
    // granularity, truncation by some SMB servers), record (ondisk, intended) for the next scan to convert with,
    // instead of brute-forcing it with a ±2s tolerance. Same approach as syncthing's mtimeFS.
    pub(super) mtime_fixes: Mutex<Vec<(bool, String, i64, i64)>>,
    pub(super) delta_saved: AtomicU64,
}

impl<'a> Shared<'a> {
    pub(super) fn exec_other(&self, side: &Side) -> (&std::sync::Arc<dyn crate::fs::vfs::Vfs>, &std::sync::Arc<dyn crate::fs::vfs::Vfs>) {
        match side {
            Side::Target => (self.target, self.source),
            Side::Source => (self.source, self.target),
        }
    }

    pub(super) fn local_of(&self, side: &Side) -> Option<&Path> {
        match side {
            Side::Target => self.target_local.as_deref(),
            Side::Source => self.source_local.as_deref(),
        }
    }

    pub(super) fn trash_reaches(&self, side: &Side) -> bool {
        match side {
            Side::Target => self.target_trash_ok,
            Side::Source => self.source_trash_ok,
        }
    }

    /// Ensure a directory exists on `exec`, memoized per (side, rel).
    pub(super) fn ensure_dir(&self, side: &Side, exec: &std::sync::Arc<dyn crate::fs::vfs::Vfs>, rel: &str) -> std::io::Result<()> {
        if rel.is_empty() {
            return Ok(());
        }
        let key = (*side == Side::Source, rel.to_string());
        if self.mkdir_memo.lock().unwrap().contains(&key) {
            return Ok(());
        }
        exec.mkdir_all(rel)?;
        self.mkdir_memo.lock().unwrap().insert(key);
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Counters {
    pub(super) done: AtomicU64,
    pub(super) skipped: AtomicU64,
    pub(super) errors: AtomicU64,
}

/// Run one class of ops. width==1 runs sequentially on the current thread; otherwise a scoped thread pool
/// (an AtomicUsize work-ticket index rather than range splitting — one big file can't drag a worker into a long tail).
/// Not rayon: `checkpoint()` parks its caller in a 100ms sleep loop for the entire length of a
/// pause, and these are threads of our own to park. Handing them to the global pool would mean a
/// user pressing Pause pins rayon workers for as long as they like. (This used to be justified by
/// verify's blake3 occupying that pool already; it no longer does — nothing here maps a file.)
pub(super) fn run_class(class: &[&Op], width: usize, sh: &Shared, pp: &PhaseProgress, acc: &Counters) {
    if class.is_empty() {
        return;
    }
    let width = width.clamp(1, 16).min(class.len());
    let next = AtomicUsize::new(0);
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= class.len() {
                break;
            }
            let op = class[i];
            // The cooperation point between two adjacent ops (a pause spins here, a cancel exits here)
            if pp.checkpoint().is_err() {
                break;
            }
            // Per-item timing: "which file slowed this sync down" in the execution ledger can only be measured here
            let t_op = std::time::Instant::now();
            let res = exec_op(sh, op, pp);
            let bail = matches!(&res, Err(e) if crate::obs::progress::is_cancelled(e) && sh.ctx.ctl.cancelled());
            record(sh, op, res, pp, acc, t_op.elapsed().as_millis() as u64);
            if bail {
                break;
            }
        }
    };
    if width == 1 {
        work();
    } else {
        std::thread::scope(|s| {
            for _ in 0..width {
                s.spawn(&work);
            }
        });
    }
}
