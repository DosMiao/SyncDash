//! apply: execute a plan — the last of the three stages, and the only one that writes.
//!
//! Everything here is arranged around one rule: an interruption at any moment must leave the
//! target either as it was or as the plan intended, never half way. Writes stage into a temp name
//! and land by rename, both roots are locked for the duration, and what is overwritten or deleted
//! is preserved first.
//!
//! Ops run in five classes, deepest-first for directory removal, because the order the plan
//! arrives in is not trusted — see `schedule`.
//!
//! - `ops` — what a single op does, per action
//! - `dir` — the classified directory delete, which has to decide *why* a directory survived
//! - `delta` — in-place patching for large updates, so an edit does not re-send the file
//! - `preserve` — where the previous content goes before it is replaced
//! - `schedule` — the worker pool and the class ordering
//! - `ledger` — what each op did, to the event stream and the run record
//! - `platform` — the cfg-gated mtime/mode primitives

pub mod delta;
pub mod dir;
pub mod ledger;
pub mod ops;
pub mod platform;
pub mod preserve;
pub mod schedule;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::model::event::Phase;
use crate::model::plan::{Action, Op, Side};
use crate::obs::progress::{ApplyOutcome, PhaseProgress, RunCtx};

use preserve::default_trash;
use schedule::{run_class, Counters, Shared};

/// Above this size, skip the in-memory delta path (don't read several GB into memory)
const DELTA_MEM_CAP: u64 = 1024 * 1024 * 1024;
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
    /// Delta updates for local/mounted disks (see the comments on update_with_delta; off by default)
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

pub fn apply(ops: &[Op], source_root: &Path, target_root: &Path, opt: &ApplyOptions) -> (u64, u64, u64) {
    apply_with(ops, source_root, target_root, opt, &RunCtx::null()).into_tuple()
}

/// The path-shaped entry: wraps both roots in LocalVfs and runs the one generic lane.
/// Kept so every existing caller (and test) works unchanged — local behavior through
/// the VFS lane is pinned by the whole apply test suite passing as-is.
pub fn apply_with(
    ops: &[Op],
    source_root: &Path,
    target_root: &Path,
    opt: &ApplyOptions,
    ctx: &RunCtx,
) -> ApplyOutcome {
    let sv: std::sync::Arc<dyn crate::fs::vfs::Vfs> =
        std::sync::Arc::new(crate::fs::vfs::local::LocalVfs::new(source_root.to_path_buf()));
    let tv: std::sync::Arc<dyn crate::fs::vfs::Vfs> =
        std::sync::Arc::new(crate::fs::vfs::local::LocalVfs::new(target_root.to_path_buf()));
    apply_vfs(ops, &sv, &tv, opt, ctx)
}

/// v0.9 M1 → v0.10 VFS: the execution body with progress/cancel/pause, now over a
/// backend pair. Five serial phases: Moves → **Copy/Update (parallel)** → Chmod →
/// Delete → DeleteDir (deepest-first within the class). Updates with delta enabled
/// stay in the serial lane — update_with_delta reads both copies into memory (≤1GB cap),
/// and 4 parallel workers × a 2GB peak is not acceptable.
pub fn apply_vfs(
    ops: &[Op],
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &ApplyOptions,
    ctx: &RunCtx,
) -> ApplyOutcome {
    // Apply's totals are known before it starts: the UI percentage formula is valid from t=0
    let items_total = ops.iter().filter(|o| !matches!(o.action, Action::Conflict | Action::Note)).count() as u64;
    let bytes_total: u64 = ops
        .iter()
        .filter(|o| matches!(o.action, Action::Copy | Action::Update) && o.link.is_none())
        .filter_map(|o| o.size)
        .sum();
    let pp = PhaseProgress::begin(ctx, Phase::Apply, None, items_total, bytes_total);
    let acc = Counters::default();

    // Conflict/Note are report-only and unrelated to the execution phases — get them out of the way first
    for op in ops {
        match op.action {
            Action::Conflict => {
                println!("CONFLICT  {} ({})", op.path, op.reason);
                acc.skipped.fetch_add(1, Ordering::Relaxed);
            }
            Action::Note => {
                println!("NOTE      {} ({} from={})", op.path, op.reason, op.from.clone().unwrap_or_default());
                acc.skipped.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    if opt.dry_run {
        for op in ops.iter().filter(|o| !matches!(o.action, Action::Conflict | Action::Note)) {
            if opt.verbose {
                let label = format!(
                    "[{}] {:?} {}",
                    if op.side == Side::Target { "target" } else { "source" },
                    op.action,
                    op.path
                );
                println!("DRY  {label}  ({})", op.reason);
            }
            acc.skipped.fetch_add(1, Ordering::Relaxed);
        }
        return ApplyOutcome {
            done: 0,
            skipped: acc.skipped.load(Ordering::Relaxed),
            errors: 0,
            bytes_copied: 0,
            cancelled: false,
        };
    }

    let source_local = source.as_local().map(|p| p.to_path_buf());
    let target_local = target.as_local().map(|p| p.to_path_buf());
    let source_trash_ok = source.caps().local_trash;
    let target_trash_ok = target.caps().local_trash;

    // The FFS dir_lock idea: lock both roots (with a heartbeat) before touching anything, so two machines cannot apply to the same directory at once.
    // Pause spins on 100ms instead of suspending and returning precisely so these two locks' heartbeat threads keep beating while paused.
    // The lock speaks the VFS: local and sftp roots get the same mutual exclusion.
    let _lock_guard: (crate::fs::lock::RootLock, crate::fs::lock::RootLock) = {
        let ls = match crate::fs::lock::RootLock::acquire_vfs(source) {
            Ok(l) => l,
            Err(e) => {
                crate::log_error!("apply", "cannot lock source root: {e}");
                return ApplyOutcome { done: 0, skipped: ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false };
            }
        };
        let lt = match crate::fs::lock::RootLock::acquire_vfs(target) {
            Ok(l) => l,
            Err(e) => {
                crate::log_error!("apply", "cannot lock target root: {e}");
                return ApplyOutcome { done: 0, skipped: ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false };
            }
        };
        (ls, lt)
    };

    let sh = Shared {
        opt,
        ctx,
        source,
        target,
        source_local,
        target_local,
        source_trash_ok,
        target_trash_ok,
        trash: opt.trash.clone().unwrap_or_else(default_trash),
        remote_keep_rel: format!(
            "{}/trash/{}",
            crate::foundation::names::APP_DIR,
            crate::foundation::time::now_ms()
        ),
        ver_source: Mutex::new(None),
        ver_target: Mutex::new(None),
        mkdir_memo: Mutex::new(std::collections::HashSet::new()),
        mtime_fixes: Mutex::new(Vec::new()),
        delta_saved: AtomicU64::new(0),
    };

    // Split into phases (input order is not trusted)
    let mut moves: Vec<&Op> = Vec::new();
    let mut copies: Vec<&Op> = Vec::new();
    let mut copies_delta: Vec<&Op> = Vec::new();
    let mut chmods: Vec<&Op> = Vec::new();
    let mut deletes: Vec<&Op> = Vec::new();
    let mut deldirs: Vec<&Op> = Vec::new();
    for op in ops {
        match op.action {
            Action::Move => moves.push(op),
            Action::Copy | Action::Update => {
                if opt.delta && op.action == Action::Update && op.link.is_none() {
                    copies_delta.push(op);
                } else {
                    copies.push(op);
                }
            }
            Action::Chmod => chmods.push(op),
            Action::Delete => deletes.push(op),
            Action::DeleteDir => deldirs.push(op),
            Action::Conflict | Action::Note => {}
        }
    }
    // Delete deep directories first, so a parent has a chance to become empty
    deldirs.sort_by_key(|o| std::cmp::Reverse(o.path.matches('/').count()));

    // The same (side, path) appearing twice (a plan shouldn't generate that, but flipping direction by hand can) → parallel would race on the write, so force sequential
    let mut seen = std::collections::HashSet::new();
    let has_dup = copies.iter().any(|o| !seen.insert((o.side == Side::Source, o.path.as_str())));
    let width = if has_dup { 1 } else { opt.parallel };

    run_class(&moves, 1, &sh, &pp, &acc);
    run_class(&copies, width, &sh, &pp, &acc);
    run_class(&copies_delta, 1, &sh, &pp, &acc);
    run_class(&chmods, 1, &sh, &pp, &acc);
    run_class(&deletes, 1, &sh, &pp, &acc);
    run_class(&deldirs, 1, &sh, &pp, &acc);

    // serial wrap-up
    let mtime_fixes = std::mem::take(&mut *sh.mtime_fixes.lock().unwrap());
    if !mtime_fixes.is_empty() {
        let mut src_fix = Vec::new();
        let mut tgt_fix = Vec::new();
        for (is_source, rel, ondisk, intended) in mtime_fixes {
            if is_source {
                src_fix.push((rel, ondisk, intended));
            } else {
                tgt_fix.push((rel, ondisk, intended));
            }
        }
        // Keyed by identity(): a local root's identity is its path string, so the
        // pre-VFS correction files keep working; a remote root gets its own table
        if !src_fix.is_empty() {
            crate::store::mtimefix::record_by_key(&sh.source.identity(), &src_fix);
        }
        if !tgt_fix.is_empty() {
            crate::store::mtimefix::record_by_key(&sh.target.identity(), &tgt_fix);
        }
    }
    let delta_saved = sh.delta_saved.load(Ordering::Relaxed);
    if delta_saved > 0 {
        println!("delta: {} not re-written", crate::foundation::fmt::human_bytes(delta_saved));
    }
    let any_remote_side = sh.source_local.is_none() || sh.target_local.is_none();
    let (src_local_path, tgt_local_path) = (sh.source_local.clone(), sh.target_local.clone());
    if let Some(w) = sh.ver_source.into_inner().unwrap() {
        let side_ops: Vec<Op> = ops.iter().filter(|o| o.side == Side::Source).cloned().collect();
        if let Ok(Some(id)) = w.finish(&side_ops) {
            if let Some(root) = &src_local_path {
                println!("version saved: {} (id {id})", root.join(crate::foundation::names::VERSION_STORE_DIR).display());
            }
        }
    }
    if let Some(w) = sh.ver_target.into_inner().unwrap() {
        let side_ops: Vec<Op> = ops.iter().filter(|o| o.side == Side::Target).cloned().collect();
        if let Ok(Some(id)) = w.finish(&side_ops) {
            if let Some(root) = &tgt_local_path {
                println!("version saved: {} (id {id})", root.join(crate::foundation::names::VERSION_STORE_DIR).display());
            }
        }
    }
    let done = acc.done.load(Ordering::Relaxed);
    if !opt.versioning && done > 0 {
        println!("trash (deleted/overwritten files kept at): {}", sh.trash.display());
    }
    if done > 0 && any_remote_side {
        println!("remote retention (originals renamed on the far side): <root>/{}", sh.remote_keep_rel);
    }
    ApplyOutcome {
        done,
        skipped: acc.skipped.load(Ordering::Relaxed),
        errors: acc.errors.load(Ordering::Relaxed),
        bytes_copied: pp.counts().2,
        cancelled: ctx.ctl.cancelled(),
    }
}
