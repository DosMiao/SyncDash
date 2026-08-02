//! Apply-session coordination after plan validation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::model::event::Phase;
use crate::model::plan::{Action, Op, Side};
use crate::obs::progress::{ApplyOutcome, PhaseProgress, RunCtx};

use super::execute::preservation::default_trash;
use super::execute::schedule::{run_class, Counters, Shared};
use super::lease::ApplyLeaseGuard;
use super::ApplyOptions;

/// Execute five ordered mutation classes over a backend pair with progress and cancellation.
/// Delta updates remain serial because each may hold two files up to the 1 GiB delta cap in memory.
pub(super) fn execute(
    ops: &[Op],
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &ApplyOptions,
    ctx: &RunCtx,
) -> ApplyOutcome {
    let validation = super::validation::validate_operation_paths(ops).and_then(|()| {
        super::validation::validate_operation_name_rules(
            ops,
            source.caps().name_rules,
            target.caps().name_rules,
        )
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
    let items_total = ops.iter().filter(|o| o.action.is_executable()).count() as u64;
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
        for op in ops.iter().filter(|o| o.action.is_executable()) {
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
        .is_some_and(|root| crate::foundation::volume::same_device(root.display_path(), &trash));
    let target_trash_ok = target_local_root
        .as_ref()
        .is_some_and(|root| crate::foundation::volume::same_device(root.display_path(), &trash));

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
    let width = super::copy_width(opt.parallel, &source.caps(), &target.caps(), has_dup);

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
    super::reporting::report_preservation_routes(&sh);
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
