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
pub mod delta;
pub mod dir;
pub mod ledger;
pub mod ops;
pub mod preserve;
pub mod schedule;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::foundation::names::windows_name_fault;
use crate::fs::vfs::NameRules;
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

fn in_root_retention_display(sh: &Shared<'_>, side: &Side) -> String {
    if let Some(root) = sh.local_root_of(side) {
        root.display_path()
            .join(crate::foundation::path::to_native(&sh.in_root_keep_rel))
            .display()
            .to_string()
    } else {
        let exec = match side {
            Side::Source => sh.source,
            Side::Target => sh.target,
        };
        format!(
            "{}/{}",
            exec.display().trim_end_matches('/'),
            sh.in_root_keep_rel
        )
    }
}

fn report_preservation_routes(sh: &Shared<'_>) {
    use crate::model::event::LogLevel;

    if sh.central_preservation_used() {
        sh.ctx.log(
            LogLevel::Info,
            "apply",
            format!(
                "trash (central; deleted/overwritten originals kept at): {}",
                sh.central_trash_root
                    .as_ref()
                    .expect("central preservation records its root")
                    .display_path()
                    .display()
            ),
        );
    }
    for (side, label) in [(Side::Source, "source"), (Side::Target, "target")] {
        if sh.in_root_preservation_used(&side) {
            sh.ctx.log(
                LogLevel::Info,
                "apply",
                format!(
                    "trash ({label} in-root; deleted/overwritten originals kept at): {}",
                    in_root_retention_display(sh, &side)
                ),
            );
        }
    }
}

/// Plans can come from files or another process, so every executable relative path is validated at
/// the last common boundary before any backend is opened. The same boundary rejects traversal,
/// platform prefixes, and SyncDash's reserved metadata namespace for every backend.
fn mutates_path(op: &Op) -> bool {
    matches!(
        op.action,
        Action::Copy | Action::Update | Action::Move | Action::Delete | Action::Chmod
    )
}

fn is_valid_move_identity_digest(hash: &str) -> bool {
    let digest = hash.strip_prefix('~').unwrap_or(hash);
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_ordered_move_source_recreation(ops: &[Op], moving: &Op, mutation: &Op) -> bool {
    // `Update` always reads this same relative path from `other`, i.e. the opposite root. The
    // scheduler runs the whole Move class before Update regardless of serialized plan order, so
    // this is a deliberate consume-then-recreate chain rather than two writers racing one name.
    // Reasons are deliberately excluded: imported plans must not gain authority by spoofing text.
    moving.action == Action::Move
        && mutation.action == Action::Update
        && moving.side == mutation.side
        && moving.from.as_deref() == Some(mutation.path.as_str())
        && mutation.from.is_none()
        && ops
            .iter()
            .filter(|candidate| {
                candidate.action == Action::Move
                    && candidate.side == moving.side
                    && candidate.from == moving.from
            })
            .count()
            == 1
        && ops
            .iter()
            .filter(|candidate| {
                mutates_path(candidate)
                    && candidate.side == mutation.side
                    && candidate.path == mutation.path
            })
            .count()
            == 1
}

fn validate_operation_paths(ops: &[Op]) -> Result<(), String> {
    let mut mutations = std::collections::HashSet::new();
    let mut move_sources = std::collections::HashSet::new();
    for op in ops
        .iter()
        .filter(|op| !matches!(op.action, Action::Conflict | Action::Note))
    {
        let operation_path = crate::foundation::path::RootRelativePath::try_from(op.path.as_str())
            .map_err(|_| format!("unsafe operation path: {}", op.path))?;
        if crate::foundation::names::is_internal_artifact_path(&operation_path) {
            return Err(format!(
                "operation path is reserved for SyncDash metadata: {}",
                op.path
            ));
        }
        if matches!(op.action, Action::Move) {
            let from = op
                .from
                .as_deref()
                .ok_or_else(|| format!("move operation is missing its source path: {}", op.path))?;
            let source_path = crate::foundation::path::RootRelativePath::try_from(from)
                .map_err(|_| format!("unsafe move source path: {from}"))?;
            if crate::foundation::names::is_internal_artifact_path(&source_path) {
                return Err(format!(
                    "move source path is reserved for SyncDash metadata: {from}"
                ));
            }
            if from == op.path {
                return Err(format!("move source and destination are identical: {from}"));
            }
            if op.size.is_none() {
                return Err(format!(
                    "move operation has no source-size evidence: {from}"
                ));
            }
            if op.hash.is_none() && op.mtime_ms.is_none() && op.link.is_none() {
                return Err(format!(
                    "move operation has neither content, mtime, nor symlink-target evidence: {from}"
                ));
            }
            if op
                .hash
                .as_deref()
                .is_some_and(|hash| !is_valid_move_identity_digest(hash))
            {
                return Err(format!(
                    "move operation has an invalid content digest: {from}"
                ));
            }
            if op.link.is_some() && op.hash.is_some() {
                return Err(format!(
                    "move operation ambiguously carries both file and symlink evidence: {from}"
                ));
            }
            let source_key = (op.side == Side::Source, from);
            if !move_sources.insert(source_key) {
                return Err(format!(
                    "duplicate move source for {} path: {from}",
                    if op.side == Side::Source {
                        "source"
                    } else {
                        "target"
                    }
                ));
            }
            let planned_recreation = ops.iter().any(|mutation| {
                mutation.path == from && is_ordered_move_source_recreation(ops, op, mutation)
            });
            if mutations.contains(&source_key) && !planned_recreation {
                return Err(format!(
                    "move source is also mutated by another operation on the same side: {from}"
                ));
            }
        }
        if mutates_path(op) {
            let mutation_key = (op.side == Side::Source, op.path.as_str());
            let planned_recreation = ops
                .iter()
                .any(|moving| is_ordered_move_source_recreation(ops, moving, op));
            if move_sources.contains(&mutation_key) && !planned_recreation {
                return Err(format!(
                    "move source is also mutated by another operation on the same side: {}",
                    op.path
                ));
            }
            if !mutations.insert(mutation_key) {
                return Err(format!(
                    "duplicate mutation for {} path: {}",
                    if op.side == Side::Source {
                        "source"
                    } else {
                        "target"
                    },
                    op.path
                ));
            }
        }
    }
    Ok(())
}

/// Imported plans can bypass comparison, so known Windows naming semantics are enforced again at
/// the last boundary that sees both the operation and the actual backends it will touch.
fn validate_operation_name_rules(
    ops: &[Op],
    source_rules: NameRules,
    target_rules: NameRules,
) -> Result<(), String> {
    for operation in ops
        .iter()
        .filter(|operation| !matches!(operation.action, Action::Conflict | Action::Note))
    {
        let (executing_rules, reading_rules) = match operation.side {
            Side::Source => (source_rules, target_rules),
            Side::Target => (target_rules, source_rules),
        };
        let creates_name = matches!(operation.action, Action::Copy | Action::Move);
        let reads_other_root = matches!(operation.action, Action::Copy | Action::Update);

        for path in [Some(operation.path.as_str()), operation.from.as_deref()]
            .into_iter()
            .flatten()
        {
            let Some((fault, reason)) = windows_name_fault(path) else {
                continue;
            };
            let unsafe_on_executing_root = executing_rules == NameRules::Windows
                && (fault.changes_addressed_path() || creates_name);
            let unsafe_on_reading_root = reads_other_root
                && reading_rules == NameRules::Windows
                && fault.changes_addressed_path();
            if unsafe_on_executing_root || unsafe_on_reading_root {
                let affected_root = match (unsafe_on_executing_root, unsafe_on_reading_root) {
                    (true, true) => "executing and reading",
                    (true, false) => "executing",
                    (false, true) => "reading",
                    (false, false) => unreachable!(),
                };
                return Err(format!(
                    "operation path {path:?} is unsafe on the {affected_root} root: {reason}"
                ));
            }
        }
    }
    Ok(())
}

pub(super) struct ApplyLeaseGuard {
    locks: (crate::fs::lock::RootLock, crate::fs::lock::RootLock),
    source_lost: std::sync::Arc<AtomicBool>,
    target_lost: std::sync::Arc<AtomicBool>,
    stop_watcher: std::sync::Arc<AtomicBool>,
    loss_announced: std::sync::Arc<AtomicBool>,
    ctx: RunCtx,
    watcher: Option<std::thread::JoinHandle<()>>,
}

fn lease_loss_error() -> std::io::Error {
    std::io::Error::other("root lock ownership was lost; apply stopped before the next mutation")
}

impl ApplyLeaseGuard {
    fn new(
        source: crate::fs::lock::RootLock,
        target: crate::fs::lock::RootLock,
        ctx: &RunCtx,
    ) -> Self {
        let source_lost = source.lease_loss_signal();
        let target_lost = target.lease_loss_signal();
        let stop_watcher = std::sync::Arc::new(AtomicBool::new(false));
        let loss_announced = std::sync::Arc::new(AtomicBool::new(false));
        let watcher_source = source_lost.clone();
        let watcher_target = target_lost.clone();
        let watcher_stop = stop_watcher.clone();
        let watcher_announced = loss_announced.clone();
        let watcher_ctx = ctx.clone();
        let watcher = std::thread::spawn(move || {
            while !watcher_stop.load(Ordering::Acquire) {
                if watcher_source.load(Ordering::Acquire) || watcher_target.load(Ordering::Acquire)
                {
                    watcher_ctx.ctl.set_paused(false);
                    if !watcher_announced.swap(true, Ordering::AcqRel) {
                        watcher_ctx.log(
                            crate::model::event::LogLevel::Error,
                            "apply",
                            "root lock ownership was lost; stopping before further mutations",
                        );
                    }
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        Self {
            locks: (source, target),
            source_lost,
            target_lost,
            stop_watcher,
            loss_announced,
            ctx: ctx.clone(),
            watcher: Some(watcher),
        }
    }

    pub(super) fn lost(&self) -> bool {
        self.source_lost.load(Ordering::Acquire) || self.target_lost.load(Ordering::Acquire)
    }

    fn announce_loss(&self) {
        self.ctx.ctl.set_paused(false);
        if !self.loss_announced.swap(true, Ordering::AcqRel) {
            self.ctx.log(
                crate::model::event::LogLevel::Error,
                "apply",
                "root lock ownership was lost; stopping before further mutations",
            );
        }
    }

    pub(super) fn checkpoint(&self, pp: &PhaseProgress<'_>) -> std::io::Result<()> {
        if self.lost() {
            self.announce_loss();
            return Err(lease_loss_error());
        }
        pp.checkpoint()?;
        if self.lost() {
            self.announce_loss();
            return Err(lease_loss_error());
        }
        Ok(())
    }

    pub(super) fn check_before_mutation(&self) -> std::io::Result<()> {
        if !self.locks.0.verify_lease_identity() || !self.locks.1.verify_lease_identity() {
            self.announce_loss();
            Err(lease_loss_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for ApplyLeaseGuard {
    fn drop(&mut self) {
        self.stop_watcher.store(true, Ordering::Release);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

/// Execute five ordered mutation classes over a backend pair with progress and cancellation.
/// Delta updates remain serial because each may hold two files up to the 1 GiB delta cap in memory.
pub fn apply_vfs(
    ops: &[Op],
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &ApplyOptions,
    ctx: &RunCtx,
) -> ApplyOutcome {
    let validation = validate_operation_paths(ops).and_then(|()| {
        validate_operation_name_rules(ops, source.caps().name_rules, target.caps().name_rules)
    });
    if let Err(error) = validation {
        crate::log_error!("apply", "refusing plan before writes: {error}");
        return ApplyOutcome {
            done: 0,
            skipped: ops.len() as u64,
            errors: 1,
            bytes_copied: 0,
            cancelled: false,
        };
    }
    // Apply's totals are known before it starts: the UI percentage formula is valid from t=0
    let items_total = ops
        .iter()
        .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
        .count() as u64;
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
                println!(
                    "NOTE      {} ({} from={})",
                    op.path,
                    op.reason,
                    op.from.clone().unwrap_or_default()
                );
                acc.skipped.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    if opt.dry_run {
        for op in ops
            .iter()
            .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
        {
            if opt.verbose {
                let label = format!(
                    "[{}] {:?} {}",
                    if op.side == Side::Target {
                        "target"
                    } else {
                        "source"
                    },
                    op.action,
                    op.path
                );
                println!("DRY  {label}  ({})", op.reason);
            }
            acc.skipped.fetch_add(1, Ordering::Relaxed);
        }
        let mut out = ApplyOutcome {
            done: 0,
            skipped: acc.skipped.load(Ordering::Relaxed),
            errors: 0,
            bytes_copied: 0,
            cancelled: false,
        };
        out.cancelled = pp.finish().is_err() || ctx.ctl.cancelled();
        return out;
    }

    let source_local_root = source.local_root().cloned();
    let target_local_root = target.local_root().cloned();
    let trash = opt.trash.clone().unwrap_or_else(default_trash);
    // `local_trash` describes only the normal store. Callers may configure another path, so the
    // actual batch directory decides reachability for every retained local root. Protocol roots
    // have no local capability and therefore always retain originals inside themselves.
    let source_trash_ok = source_local_root
        .as_ref()
        .is_some_and(|root| crate::fs::vfs::local::same_device(root.display_path(), &trash));
    let target_trash_ok = target_local_root
        .as_ref()
        .is_some_and(|root| crate::fs::vfs::local::same_device(root.display_path(), &trash));

    // Lock both roots before touching either one, so two machines cannot apply to the same
    // directory concurrently. The immutable generation claim proves ownership; its heartbeat
    // timestamp remains visible during Pause but never authorizes stale takeover. Backends without
    // exclusive staged-file publication are blocked by capability preflight and fail closed here too.
    let lease_guard = {
        let ls = match crate::fs::lock::RootLock::acquire_vfs(source) {
            Ok(l) => l,
            Err(e) => {
                crate::log_error!("apply", "cannot lock source root: {e}");
                return ApplyOutcome {
                    done: 0,
                    skipped: ops.len() as u64,
                    errors: 1,
                    bytes_copied: 0,
                    cancelled: false,
                };
            }
        };
        let lt = match crate::fs::lock::RootLock::acquire_vfs(target) {
            Ok(l) => l,
            Err(e) => {
                crate::log_error!("apply", "cannot lock target root: {e}");
                return ApplyOutcome {
                    done: 0,
                    skipped: ops.len() as u64,
                    errors: 1,
                    bytes_copied: 0,
                    cancelled: false,
                };
            }
        };
        ApplyLeaseGuard::new(ls, lt, ctx)
    };

    let central_trash_root = if source_trash_ok || target_trash_ok {
        match crate::fs::local_root::LocalRoot::create(trash) {
            Ok(root) => Some(root),
            Err(error) => {
                crate::log_error!("apply", "cannot open the central trash root: {error}");
                return ApplyOutcome {
                    done: 0,
                    skipped: ops.len() as u64,
                    errors: 1,
                    bytes_copied: 0,
                    cancelled: false,
                };
            }
        }
    } else {
        None
    };

    let sh = Shared {
        opt,
        ctx,
        lease: &lease_guard,
        source,
        target,
        source_local_root,
        target_local_root,
        source_trash_ok,
        target_trash_ok,
        central_trash_root,
        in_root_keep_rel: format!(
            "{}/trash/{}",
            crate::foundation::names::APP_DIR,
            crate::foundation::time::now_ms()
        ),
        central_preserved: std::sync::atomic::AtomicBool::new(false),
        source_in_root_preserved: std::sync::atomic::AtomicBool::new(false),
        target_in_root_preserved: std::sync::atomic::AtomicBool::new(false),
        ver_source: Mutex::new(None),
        ver_target: Mutex::new(None),
        mkdir_memo: Mutex::new(std::collections::HashSet::new()),
        mtime_fixes: Mutex::new(Vec::new()),
        delta_saved: AtomicU64::new(0),
    };

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
    let has_dup = copies
        .iter()
        .any(|o| !seen.insert((o.side == Side::Source, o.path.as_str())));
    let width = copy_width(opt.parallel, &source.caps(), &target.caps(), has_dup);

    run_class(&moves, 1, &sh, &pp, &acc);
    run_class(&copies, width, &sh, &pp, &acc);
    run_class(&copies_delta, 1, &sh, &pp, &acc);
    run_class(&chmods, 1, &sh, &pp, &acc);
    run_class(&deletes, 1, &sh, &pp, &acc);
    run_class(&deldirs, 1, &sh, &pp, &acc);

    let lease_healthy_for_wrapup = lease_guard.check_before_mutation().is_ok();
    let mtime_fixes = std::mem::take(&mut *sh.mtime_fixes.lock().unwrap());
    if lease_healthy_for_wrapup && !mtime_fixes.is_empty() {
        let mut src_fix = Vec::new();
        let mut tgt_fix = Vec::new();
        for (is_source, rel, ondisk, intended) in mtime_fixes {
            if is_source {
                src_fix.push((rel, ondisk, intended));
            } else {
                tgt_fix.push((rel, ondisk, intended));
            }
        }
        // Local tables keep the historical path-derived filename but bind their header to the
        // physical volume. Network roots remain keyed by their credential-free VFS identity.
        if !src_fix.is_empty() {
            if let Some(root) = sh.source_local_root.as_ref() {
                let identity =
                    crate::store::localid::LocalScanStateIdentity::for_root(root.display_path());
                crate::store::mtimefix::record_local(&identity, &src_fix);
            } else {
                crate::store::mtimefix::record_by_key(&sh.source.identity(), &src_fix);
            }
        }
        if !tgt_fix.is_empty() {
            if let Some(root) = sh.target_local_root.as_ref() {
                let identity =
                    crate::store::localid::LocalScanStateIdentity::for_root(root.display_path());
                crate::store::mtimefix::record_local(&identity, &tgt_fix);
            } else {
                crate::store::mtimefix::record_by_key(&sh.target.identity(), &tgt_fix);
            }
        }
    }
    let delta_saved = sh.delta_saved.load(Ordering::Relaxed);
    if delta_saved > 0 {
        println!(
            "delta: {} not re-written",
            crate::foundation::fmt::human_bytes(delta_saved)
        );
    }
    report_preservation_routes(&sh);
    let finalize_versions = lease_healthy_for_wrapup;
    let (source_local_root, target_local_root) =
        (sh.source_local_root.clone(), sh.target_local_root.clone());
    if let Some(w) = sh.ver_source.into_inner().unwrap() {
        let side_ops: Vec<Op> = ops
            .iter()
            .filter(|o| o.side == Side::Source)
            .cloned()
            .collect();
        if finalize_versions && lease_guard.check_before_mutation().is_ok() {
            match w.finish(&side_ops, opt.fsync, || lease_guard.check_before_mutation()) {
                Ok(Some(id)) => {
                    if let Some(root) = &source_local_root {
                        println!(
                            "version saved: {} (id {id})",
                            root.display_path()
                                .join(crate::foundation::names::VERSION_STORE_DIR)
                                .display()
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    acc.errors.fetch_add(1, Ordering::Relaxed);
                    crate::log_error!(
                        "apply",
                        "could not finalize source version history: {error}"
                    );
                }
            }
        } else if let Some(root) = &source_local_root {
            crate::log_warn!(
                "apply",
                "leaving the unindexed version staging directory under {} because root-lock ownership was lost",
                root.display_path()
                    .join(crate::foundation::names::VERSION_STORE_DIR)
                    .display()
            );
        }
    }
    if let Some(w) = sh.ver_target.into_inner().unwrap() {
        let side_ops: Vec<Op> = ops
            .iter()
            .filter(|o| o.side == Side::Target)
            .cloned()
            .collect();
        if finalize_versions && lease_guard.check_before_mutation().is_ok() {
            match w.finish(&side_ops, opt.fsync, || lease_guard.check_before_mutation()) {
                Ok(Some(id)) => {
                    if let Some(root) = &target_local_root {
                        println!(
                            "version saved: {} (id {id})",
                            root.display_path()
                                .join(crate::foundation::names::VERSION_STORE_DIR)
                                .display()
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    acc.errors.fetch_add(1, Ordering::Relaxed);
                    crate::log_error!(
                        "apply",
                        "could not finalize target version history: {error}"
                    );
                }
            }
        } else if let Some(root) = &target_local_root {
            crate::log_warn!(
                "apply",
                "leaving the unindexed version staging directory under {} because root-lock ownership was lost",
                root.display_path()
                    .join(crate::foundation::names::VERSION_STORE_DIR)
                    .display()
            );
        }
    }
    let done = acc.done.load(Ordering::Relaxed);
    let lease_lost = lease_guard.lost();
    let mut out = ApplyOutcome {
        done,
        skipped: acc.skipped.load(Ordering::Relaxed),
        errors: acc.errors.load(Ordering::Relaxed)
            + u64::from(lease_lost && !acc.lease_failure_recorded.load(Ordering::Relaxed)),
        bytes_copied: pp.counts().2,
        cancelled: ctx.ctl.cancelled(),
    };
    if lease_lost {
        pp.fail();
        out.cancelled = false;
    } else {
        out.cancelled = pp.finish().is_err() || ctx.ctl.cancelled();
    }
    out
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    use crate::fs::vfs::memory::MemVfs;
    use crate::fs::vfs::Vfs;
    use std::sync::{Arc, Mutex};

    fn copy_op(path: &str, size: usize) -> Op {
        Op {
            side: Side::Target,
            action: Action::Copy,
            path: path.to_owned(),
            from: None,
            size: Some(size as u64),
            mtime_ms: Some(1),
            hash: None,
            link: None,
            mode: None,
            reason: "lease test".to_owned(),
        }
    }

    fn remove_current_claim(root: &MemVfs) {
        let claims = root
            .read_dir_names("")
            .unwrap()
            .into_iter()
            .filter(|(name, kind)| {
                *kind == crate::fs::vfs::VfsEntryKind::File
                    && name
                        .as_str()
                        .starts_with(crate::foundation::names::LOCK_NAME)
                    && name.as_str().contains(".claim.")
            })
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(claims.len(), 1);
        root.remove_file(claims[0].as_str()).unwrap();
    }

    fn options() -> ApplyOptions {
        ApplyOptions {
            dry_run: false,
            parallel: 1,
            fsync: false,
            ..ApplyOptions::default()
        }
    }

    #[test]
    fn lease_loss_during_copy_never_publishes_the_staged_destination() {
        let source = Arc::new(MemVfs::new("lease-loss-copy-source"));
        let target = Arc::new(MemVfs::new("lease-loss-copy-target"));
        let content = vec![7u8; 512 * 1024];
        source.seed_bytes("large.bin", &content, 1);

        let hook_source = source.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let hook_fired = fired.clone();
        source.set_read_hook(move |path, _| {
            if path == "large.bin" && !hook_fired.swap(true, Ordering::AcqRel) {
                remove_current_claim(&hook_source);
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
        });

        let source_vfs: Arc<dyn Vfs> = source;
        let target_vfs: Arc<dyn Vfs> = target.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let event_store = events.clone();
        let ctx = RunCtx::new(
            crate::obs::progress::RunCtl::new(),
            Arc::new(move |event| event_store.lock().unwrap().push(event)),
        );
        let out = apply_vfs(
            &[copy_op("large.bin", content.len())],
            &source_vfs,
            &target_vfs,
            &options(),
            &ctx,
        );

        assert!(fired.load(Ordering::Acquire));
        assert_eq!(out.done, 0);
        assert_eq!(out.errors, 1);
        assert!(
            !out.cancelled,
            "lease loss is a safety failure, not user cancel"
        );
        assert!(target.stat("large.bin").unwrap().is_none());
        assert!(events.lock().unwrap().iter().any(|event| matches!(
            event,
            crate::model::event::ProgressEvent::PhaseEnd {
                phase: Phase::Apply,
                status: crate::model::event::PhaseStatus::Failed,
                ..
            }
        )));
    }

    #[test]
    fn lease_loss_after_one_commit_blocks_the_next_operation() {
        let source = Arc::new(MemVfs::new("lease-loss-next-source"));
        let target = Arc::new(MemVfs::new("lease-loss-next-target"));
        source.seed_bytes("first.bin", b"first", 1);
        source.seed_bytes("second.bin", b"second", 1);

        let hook_source = source.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let hook_fired = fired.clone();
        target.set_commit_hook(move |path| {
            if path == "first.bin" && !hook_fired.swap(true, Ordering::AcqRel) {
                remove_current_claim(&hook_source);
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
        });

        let source_vfs: Arc<dyn Vfs> = source;
        let target_vfs: Arc<dyn Vfs> = target.clone();
        let out = apply_vfs(
            &[copy_op("first.bin", 5), copy_op("second.bin", 6)],
            &source_vfs,
            &target_vfs,
            &options(),
            &RunCtx::null(),
        );

        assert!(fired.load(Ordering::Acquire));
        assert_eq!(out.done, 1);
        assert_eq!(out.errors, 1);
        assert!(
            !out.cancelled,
            "lease loss is a safety failure, not user cancel"
        );
        assert!(target.stat("first.bin").unwrap().is_some());
        assert!(target.stat("second.bin").unwrap().is_none());
    }

    #[test]
    fn destination_appearing_at_publication_is_never_replaced() {
        let source = Arc::new(MemVfs::new("publish-race-source"));
        let target = Arc::new(MemVfs::new("publish-race-target"));
        source.seed_bytes("file.bin", b"planned content", 1);

        let hook_target = target.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let hook_fired = fired.clone();
        target.set_noreplace_pre_publish_hook(move |path| {
            if path == "file.bin" && !hook_fired.swap(true, Ordering::AcqRel) {
                hook_target.seed_bytes(path, b"external writer", 2);
            }
        });

        let source_vfs: Arc<dyn Vfs> = source;
        let target_vfs: Arc<dyn Vfs> = target.clone();
        let out = apply_vfs(
            &[copy_op("file.bin", b"planned content".len())],
            &source_vfs,
            &target_vfs,
            &options(),
            &RunCtx::null(),
        );

        assert!(fired.load(Ordering::Acquire));
        assert_eq!((out.done, out.errors), (0, 1));
        let mut reader = target.open_read("file.bin").unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut bytes).unwrap();
        assert_eq!(bytes, b"external writer");
    }

    #[test]
    fn published_file_with_wrong_permissions_is_not_reported_as_success() {
        let source = Arc::new(MemVfs::new("mode-failure-source"));
        let target = Arc::new(
            MemVfs::new("mode-failure-target")
                .failing_commit_mode(crate::fs::vfs::error::VfsErrorKind::PermissionDenied),
        );
        source.seed_bytes("file.bin", b"content", 1);
        let mut operation = copy_op("file.bin", b"content".len());
        operation.mode = Some(0o600);

        let source_vfs: Arc<dyn Vfs> = source;
        let target_vfs: Arc<dyn Vfs> = target.clone();
        let out = apply_vfs(
            &[operation],
            &source_vfs,
            &target_vfs,
            &options(),
            &RunCtx::null(),
        );

        assert_eq!((out.done, out.errors), (0, 1));
        assert!(target.stat("file.bin").unwrap().is_some());
    }

    #[test]
    fn plans_cannot_mutate_internal_artifact_paths() {
        let source = Arc::new(MemVfs::new("reserved-path-source"));
        let target = Arc::new(MemVfs::new("reserved-path-target"));
        source.seed_bytes("nested/.syncdash/cache.bin", b"content", 1);
        let operation = copy_op("nested/.syncdash/cache.bin", b"content".len());

        let source_vfs: Arc<dyn Vfs> = source.clone();
        let target_vfs: Arc<dyn Vfs> = target.clone();
        let out = apply_vfs(
            &[operation],
            &source_vfs,
            &target_vfs,
            &options(),
            &RunCtx::null(),
        );

        assert_eq!((out.done, out.skipped, out.errors), (0, 1, 1));
        assert!(target.stat("nested/.syncdash/cache.bin").unwrap().is_none());
        assert!(source
            .stat(crate::foundation::names::LOCK_NAME)
            .unwrap()
            .is_none());
        assert!(target
            .stat(crate::foundation::names::LOCK_NAME)
            .unwrap()
            .is_none());

        let mut move_operation = copy_op("safe.bin", 1);
        move_operation.action = Action::Move;
        move_operation.from = Some("nested/.syncdash.tmp.claim".to_owned());
        assert!(validate_operation_paths(&[move_operation])
            .unwrap_err()
            .contains("reserved for SyncDash metadata"));
    }

    #[test]
    fn move_hash_evidence_requires_a_canonical_digest() {
        let mut operation = copy_op("moved.bin", 1);
        operation.action = Action::Move;
        operation.from = Some("original.bin".to_owned());
        operation.hash = Some("A".repeat(64));

        assert!(validate_operation_paths(&[operation])
            .unwrap_err()
            .contains("invalid content digest"));
    }

    #[test]
    fn imported_plan_cannot_bypass_windows_name_addressing_rules() {
        let source = Arc::new(MemVfs::new("windows-name-source"));
        let target = Arc::new(MemVfs::new("windows-name-target").without(|capabilities| {
            capabilities.name_rules = NameRules::Windows;
        }));
        source.seed_bytes("report:2024.pdf", b"content", 1);

        let source_vfs: Arc<dyn Vfs> = source;
        let target_vfs: Arc<dyn Vfs> = target.clone();
        let outcome = apply_vfs(
            &[copy_op("report:2024.pdf", b"content".len())],
            &source_vfs,
            &target_vfs,
            &options(),
            &RunCtx::null(),
        );

        assert_eq!((outcome.done, outcome.skipped, outcome.errors), (0, 1, 1));
        assert!(target.stat("report:2024.pdf").unwrap().is_none());
    }
}
