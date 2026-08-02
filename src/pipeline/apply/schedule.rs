//! The worker pool and the shared state one apply run hands to its threads.
//!
//! Ops are executed in classes rather than in plan order: moves before copies before deletes,
//! and directory removals deepest-first. `compare` already emits them ranked, but the input to
//! `apply` is not trusted to have come from `compare` — a plan can arrive from a package built
//! by another machine, so the ordering is re-derived here.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use super::ledger::record;
use super::ops::exec_op;
use super::{ApplyLeaseGuard, ApplyOptions};
use crate::model::plan::Op;
use crate::model::plan::Side;
use crate::obs::progress::{PhaseProgress, RunCtx};

/// Execution environment shared by the worker threads. All counters are atomic and writers sit behind locks — a worker only borrows &Shared.
pub(super) struct Shared<'a> {
    pub(super) opt: &'a ApplyOptions,
    pub(super) ctx: &'a RunCtx,
    pub(super) lease: &'a ApplyLeaseGuard,
    pub(super) source: &'a std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    pub(super) target: &'a std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    pub(super) source_local_root: Option<crate::fs::local_root::LocalRoot>,
    pub(super) target_local_root: Option<crate::fs::local_root::LocalRoot>,
    /// Whether the central trash store may take each side's deletions.
    ///
    /// Separate from local-capability presence: a mounted share or external disk supports the
    /// descriptor-local delta lane, but a move into another volume would copy every deleted file.
    pub(super) source_trash_ok: bool,
    pub(super) target_trash_ok: bool,
    pub(super) central_trash_root: Option<crate::fs::local_root::LocalRoot>,
    /// The in-root retention area for every side that cannot rename into the central store:
    /// `.syncdash/trash/<run_ms>` under that root. This includes protocol roots, mounted shares,
    /// and external local volumes.
    pub(super) in_root_keep_rel: String,
    pub(super) central_preserved: AtomicBool,
    pub(super) source_in_root_preserved: AtomicBool,
    pub(super) target_in_root_preserved: AtomicBool,
    pub(super) ver_source: Mutex<Option<crate::store::version::VersionWriter>>,
    pub(super) ver_target: Mutex<Option<crate::store::version::VersionWriter>>,
    /// Directories already ensured on each side this run (spares one round-trip per network root)
    pub(super) mkdir_memo: Mutex<std::collections::HashSet<(bool, String)>>,
    // P1-4: when the mtime the filesystem actually stored differs from the one we wanted (FAT's 2-second
    // granularity, truncation by some SMB servers), record (ondisk, intended) for the next scan to convert with,
    // instead of brute-forcing it with a ±2s tolerance. Same approach as syncthing's mtimeFS.
    pub(super) mtime_fixes: Mutex<Vec<(bool, String, i64, i64)>>,
    pub(super) delta_saved: AtomicU64,
}

impl<'a> Shared<'a> {
    pub(super) fn checkpoint(&self, pp: &PhaseProgress<'_>) -> std::io::Result<()> {
        self.lease.checkpoint(pp)
    }

    pub(super) fn check_before_mutation(&self) -> std::io::Result<()> {
        self.lease.check_before_mutation()
    }

    pub(super) fn lease_lost(&self) -> bool {
        self.lease.lost()
    }

    pub(super) fn exec_other(
        &self,
        side: &Side,
    ) -> (
        &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
        &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    ) {
        match side {
            Side::Target => (self.target, self.source),
            Side::Source => (self.source, self.target),
        }
    }

    pub(super) fn local_root_of(&self, side: &Side) -> Option<&crate::fs::local_root::LocalRoot> {
        match side {
            Side::Target => self.target_local_root.as_ref(),
            Side::Source => self.source_local_root.as_ref(),
        }
    }

    pub(super) fn trash_reaches(&self, side: &Side) -> bool {
        match side {
            Side::Target => self.target_trash_ok,
            Side::Source => self.source_trash_ok,
        }
    }

    pub(super) fn note_central_preservation(&self) {
        self.central_preserved.store(true, Ordering::Relaxed);
    }

    pub(super) fn note_in_root_preservation(&self, side: &Side) {
        match side {
            Side::Source => self.source_in_root_preserved.store(true, Ordering::Relaxed),
            Side::Target => self.target_in_root_preserved.store(true, Ordering::Relaxed),
        }
    }

    pub(super) fn central_preservation_used(&self) -> bool {
        self.central_preserved.load(Ordering::Relaxed)
    }

    pub(super) fn in_root_preservation_used(&self, side: &Side) -> bool {
        match side {
            Side::Source => self.source_in_root_preserved.load(Ordering::Relaxed),
            Side::Target => self.target_in_root_preserved.load(Ordering::Relaxed),
        }
    }

    /// Ensure a directory exists on `exec`, memoized per (side, rel).
    pub(super) fn ensure_dir(
        &self,
        side: &Side,
        exec: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
        rel: &str,
    ) -> std::io::Result<()> {
        if rel.is_empty() {
            return Ok(());
        }
        let key = (*side == Side::Source, rel.to_string());
        if self.mkdir_memo.lock().unwrap().contains(&key) {
            return Ok(());
        }
        self.check_before_mutation()?;
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
    pub(super) lease_failure_recorded: AtomicBool,
}

/// Run one class of ops. width==1 runs sequentially on the current thread; otherwise a scoped thread pool
/// (an AtomicUsize work-ticket index rather than range splitting — one big file can't drag a worker into a long tail).
/// Not rayon: `checkpoint()` parks its caller in a 100ms sleep loop for the entire length of a
/// pause, and these are threads of our own to park. Handing them to the global pool would mean a
/// user pressing Pause pins rayon workers for as long as they like. (This used to be justified by
/// verify's blake3 occupying that pool already; it no longer does — nothing here maps a file.)
pub(super) fn run_class(
    class: &[&Op],
    width: usize,
    sh: &Shared,
    pp: &PhaseProgress,
    acc: &Counters,
) {
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
            if sh.checkpoint(pp).is_err() {
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
                s.spawn(work);
            }
        });
    }
}
