//! Jobs that execute in this process.
//!
//! "Local" is about the transport, not the roots: an `sftp://` target still runs here, because
//! this process does the reading and writing. What makes a job *not* local is a peer that runs
//! syncdash itself — see `remote`.


use crate::job::Job;
use crate::model::plan::{Action, Op, Plan};
use crate::model::table::Snapshot;
use crate::pipeline::{apply, compare, scan};

use super::{scan_opts, CompareOutcome};
use super::archive::refresh_archive_with;
use super::roots::resolve_root;
use std::path::Path;

/// The same pipeline, but it also hands back both snapshots (throwing them away would force the UI to scan all over again)
/// `accept_caps` = the user consented (--accept-caps / a ticked confirmation box) to the
/// NeedsAck lines of the capability report. Without consent a degraded run refuses to start.
pub fn compare_job_detailed(job: &Job, ctx: &crate::obs::progress::RunCtx, accept_caps: bool) -> std::io::Result<CompareOutcome> {
    // The single pipeline handles a single target only: multi-target jobs are derived through for_target first (the CLI run loop / the desktop target picker)
    job.validate_multi_target().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    if job.targets.len() > 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "multi-target job: resolve one target first (desktop target picker / CLI `run` loops all)",
        ));
    }
    // Resolve both roots to live backends before anything else: local stays local, a
    // translating backend mounts, a genuinely remote one connects — Auth/unreachable
    // errors surface here, never mid-scan.
    let sv = resolve_root(&job.source)?;
    let tv = resolve_root(&job.target)?;
    compare_resolved(job, &sv, &tv, ctx, accept_caps)
}

/// Compare two roots that are already open. Split out from `compare_job_detailed` so the phrase
/// layer and the comparison are separable: everything below here works on backends, not spellings,
/// which is what lets the VFS lane be exercised against an in-memory root.
pub fn compare_resolved(
    job: &Job,
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    ctx: &crate::obs::progress::RunCtx,
    accept_caps: bool,
) -> std::io::Result<CompareOutcome> {
    use crate::model::event::Phase;
    let mut opt = scan_opts(job);
    // P0-2: root reachability + mount-point marker. When the share isn't mounted, target is often an empty directory,
    // and comparing as usual yields a plan that either "wipes the other side" or "re-sends everything".
    let mut v = crate::pipeline::guard::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::pipeline::guard::roots::check_root_vfs("source", sv, job.require_marker, &mut v);
    crate::pipeline::guard::roots::check_root_vfs("target", tv, job.require_marker, &mut v);
    for w in &v.warnings {
        // v0.10: Log{Warn} replaces the Error{action:"warning"} hack —
        // with a real level, a warning no longer has to masquerade as an "error that doesn't count"
        ctx.log(crate::model::event::LogLevel::Warn, "compare", format!("warning: {w}"));
    }
    if !v.ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, v.blockers.join("; ")));
    }
    // The no-hash equality window widens to the coarser of the two backends' declared
    // mtime precision (an FTP LIST root thinks in minutes). Hash evidence is unaffected.
    let mut copts = job.compare_opts();
    copts.mtime_window_ms = copts
        .mtime_window_ms
        .max(sv.caps().mtime_precision_ms as i64)
        .max(tv.caps().mtime_precision_ms as i64);
    // The capability report: every gap between what the job asks and what the backends
    // can give is listed BEFORE any scanning — blockers refuse, NeedsAck lines demand
    // explicit consent, Info lines go to the log. Nothing degrades silently.
    let q = job.read_caps_query(copts.mtime_window_ms, sv.as_local().is_some(), tv.as_local().is_some());
    let caps_report = crate::pipeline::guard::caps::cap_report_read(&q, &sv.caps(), &tv.caps());
    {
        use crate::model::event::LogLevel;
        use crate::pipeline::guard::caps::CapSeverity;
        for i in &caps_report.items {
            let lvl = match i.severity {
                CapSeverity::Block => LogLevel::Error,
                CapSeverity::NeedsAck => LogLevel::Warn,
                CapSeverity::Info => LogLevel::Info,
            };
            ctx.log(lvl, "caps", i.render());
        }
        let blockers = caps_report.blockers();
        if !blockers.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                blockers.iter().map(|i| i.render()).collect::<Vec<_>>().join("; "),
            ));
        }
        let acks = caps_report.needs_ack();
        if !acks.is_empty() && !accept_caps {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "this run degrades on capabilities the backends lack — rerun with --accept-caps to consent:\n  {}",
                    acks.iter().map(|i| i.render()).collect::<Vec<_>>().join("\n  ")
                ),
            ));
        }
    }
    // Joint tier adjustment: a `~` sampled digest can only ever match another sampled
    // digest, so when either side cannot sample, BOTH sides read in full — a one-sided
    // upgrade would make identical files look different (a false positive, the exact
    // kind of lie this tool exists to not tell).
    if opt.sampled && !(sv.caps().ranged_read.yes() && tv.caps().ranged_read.yes()) {
        opt.sampled = false;
    }
    // Scan both sides in parallel: source and target are almost always on different disks/links (local disk vs SMB,
    // OneDrive vs an external drive), so serial execution is pure queueing — in parallel, wall clock ≈ the slower side.
    // Each side emits its own PhaseStart at the same moment, so the progress panel ticks on two rows at once.
    let (s, t) = std::thread::scope(|sc| {
        let hs = sc.spawn(|| scan::scan_root(&sv, &opt, ctx, Phase::ScanSource));
        let ht = sc.spawn(|| scan::scan_root(&tv, &opt, ctx, Phase::ScanTarget));
        (hs.join().unwrap(), ht.join().unwrap())
    });
    let (mut s, mut t) = (s?, t?);
    // The consented degradations ride on the snapshot itself — a table must say how its
    // evidence was gathered
    {
        use crate::pipeline::guard::caps::CapSeverity;
        let lines_for = |side: &str| {
            caps_report
                .items
                .iter()
                .filter(|i| i.severity != CapSeverity::Info && (i.side == side || i.side == "both"))
                .map(|i| i.render())
                .collect::<Vec<_>>()
        };
        if let Some(n) = s.header.vfs.as_mut() {
            n.degraded = lines_for("source");
        }
        if let Some(n) = t.header.vfs.as_mut() {
            n.degraded = lines_for("target");
        }
    }
    let archive = match (&job.archive, job.mode.as_str()) {
        (Some(p), "sync") if p.is_file() => Some(Snapshot::load(p)?),
        _ => None,
    };
    // compare itself is sub-second CPU work: report the phase boundary only, no internal counting
    let _pp = crate::obs::progress::PhaseProgress::begin(
        ctx,
        Phase::Compare,
        Some(format!("{} × {} entries", s.header.entry_count, t.header.entry_count)),
        0,
        0,
    );
    let plan = compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &copts);
    // Disagreement escalation: in the sampled evidence tier, a file whose digests match but whose mtimes differ by >2s may not simply be ruled identical (the knob can turn this off)
    let rr = job.rigor_resolved();
    let plan = if rr.sampled && rr.escalate {
        match (sv.as_local(), tv.as_local()) {
            (Some(sp), Some(tp)) => {
                let (sp, tp) = (sp.to_path_buf(), tp.to_path_buf());
                escalate_sampled_disagreements(job, plan, &s, &t, ctx, &sp, &tp)
            }
            _ => {
                ctx.log(
                    crate::model::event::LogLevel::Info,
                    "compare",
                    "escalation skipped: a root lives on a remote backend (full re-reads over the VFS arrive with the write lane)".to_string(),
                );
                plan
            }
        }
    } else {
        plan
    };
    Ok(CompareOutcome { plan, source: s, target: t })
}

/// The escalation rule: when the two signals fight (the sampled digest says "identical", mtime says "touched"), believe neither silently —
/// **escalate that file to a full hash on both sides** and rule again. This shrinks the blind spot of fast/standard from
/// "any change outside the sampling window" to "outside the sampling window *and* timestamp-preserving" (≈ the timestomp case).
/// The escalation set is naturally tiny (near zero on a normal tree), so reading each one in full costs next to nothing. Local pipeline only (the remote side has no cheap way to re-read).
fn escalate_sampled_disagreements(
    job: &Job,
    mut plan: Plan,
    s: &Snapshot,
    t: &Snapshot,
    ctx: &crate::obs::progress::RunCtx,
    src_root: &Path,
    tgt_root: &Path,
) -> Plan {
    use crate::model::plan::Side;
    use crate::model::event::LogLevel;
    use crate::model::table::EntryKind;
    use rayon::prelude::*;
    // The same no-hash window compare uses — a backend with coarser mtimes widens both together
    let slack_ms: i64 = job.compare_opts().mtime_window_ms;

    fn full_hash(p: &Path) -> std::io::Result<String> {
        let mut h = blake3::Hasher::new();
        h.update_mmap(p)?;
        Ok(h.finalize().to_hex().to_string())
    }
    let tmap: std::collections::HashMap<&str, &crate::model::table::Entry> = t
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .map(|e| (e.path.as_str(), e))
        .collect();
    let suspects: Vec<(&crate::model::table::Entry, &crate::model::table::Entry)> = s
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .filter_map(|se| tmap.get(se.path.as_str()).map(|te| (se, *te)))
        .filter(|(se, te)| match (&se.hash, &te.hash) {
            (Some(a), Some(b)) => a.starts_with('~') && a == b && (se.mtime_ms - te.mtime_ms).abs() > slack_ms,
            _ => false,
        })
        .collect();
    if suspects.is_empty() {
        return plan;
    }
    ctx.log(LogLevel::Info, "compare", format!("escalation: {} file(s) with equal digests but mtime differing >2s — re-verifying both sides in full", suspects.len()));
    let extra: Vec<Op> = suspects
        .par_iter()
        .filter_map(|(se, te)| {
            let hs = full_hash(&crate::foundation::path::join_native(src_root, &se.path)).ok()?;
            let ht = full_hash(&crate::foundation::path::join_native(tgt_root, &te.path)).ok()?;
            if hs == ht {
                return None; // the digest wasn't lying: only the mtime drifted
            }
            let reason = "escalated: sampled digests equal, mtime differs, full hashes differ";
            match job.mode.as_str() {
                // mirror: source wins unconditionally
                "mirror" => Some(Op { side: Side::Target, action: Action::Update, path: se.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: Some(hs), link: None, mode: None, reason: reason.into() }),
                // sync: both sides differ in content with no attribution → report the conflict honestly, a human rules
                "sync" => Some(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: None, link: None, mode: None, reason: reason.into() }),
                // enrich: update only when source is strictly newer
                _ => {
                    if se.mtime_ms > te.mtime_ms + slack_ms {
                        Some(Op { side: Side::Target, action: Action::Update, path: se.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: Some(hs), link: None, mode: None, reason: reason.into() })
                    } else {
                        None
                    }
                }
            }
        })
        .collect();
    if !extra.is_empty() {
        ctx.log(LogLevel::Warn, "compare", format!("escalation confirmed {} file(s) really do differ in content (changes outside the sampling window); added to the plan", extra.len()));
        let new_conflicts = extra.iter().filter(|o| o.action == Action::Conflict).count() as u64;
        plan.header.op_count += extra.len() as u64;
        plan.header.conflict_count += new_conflicts;
        plan.ops.extend(extra);
    }
    plan
}

/// Run the gates without executing — the GUI calls this before raising the confirmation sheet, so the refusal
/// reasons are shown to the person in full instead of landing only on a stderr nobody reads.
pub fn preflight_job(job: &Job, plan: &Plan, ops: &[Op], acknowledged: bool) -> crate::pipeline::guard::Verdict {
    crate::pipeline::guard::run_all(
        ops,
        Path::new(&plan.header.source_root),
        Path::new(&plan.header.target_root),
        plan.header.source_entries,
        plan.header.target_entries,
        &job.guards(acknowledged),
    )
}

/// v0.9 M1: apply orchestration with an event stream — the Apply phase (apply_with reports its own totals and
/// byte-by-byte progress) → the Refresh phase → the Summary terminal state.
pub fn apply_job_guarded_with(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    use crate::model::event::{Phase, ProgressEvent};
use crate::obs::progress::ApplyOutcome;
    let t0 = std::time::Instant::now();
    let refuse = |action: &str, message: String| {
        ctx.sink.emit(ProgressEvent::Error {
            phase: Phase::Apply,
            ts_ms: crate::foundation::time::now_ms(),
            path: String::new(),
            action: action.into(),
            side: "target".into(),
            message,
        });
        ApplyOutcome { done: 0, skipped: ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false }
    };
    let sv = match resolve_root(&job.source) {
        Ok(v) => v,
        Err(e) => return refuse("resolve-roots", e.to_string()),
    };
    let tv = match resolve_root(&job.target) {
        Ok(v) => v,
        Err(e) => return refuse("resolve-roots", e.to_string()),
    };
    // The plan must be the one made for THESE roots. The header carries the label the
    // scan wrote: the local (possibly translated) path for local lanes, the display
    // phrase for generic-lane roots.
    let label = |v: &std::sync::Arc<dyn crate::fs::vfs::Vfs>| {
        v.as_local().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| v.display())
    };
    if label(&sv) != plan.header.source_root || label(&tv) != plan.header.target_root {
        return refuse(
            "resolve-roots",
            format!(
                "this plan was made for '{}' → '{}' but the job resolves to '{}' → '{}' — run compare again",
                plan.header.source_root,
                plan.header.target_root,
                label(&sv),
                label(&tv)
            ),
        );
    }
    // Write-side capability report: gaps listed BEFORE anything is touched
    {
        use crate::model::event::LogLevel;
        use crate::pipeline::guard::caps::CapSeverity;
        let q = job.write_caps_query(sv.as_local().is_some(), tv.as_local().is_some());
        let wr = crate::pipeline::guard::caps::cap_report_write(&q, ops, &sv.caps(), &tv.caps());
        for i in &wr.items {
            let lvl = match i.severity {
                CapSeverity::Block => LogLevel::Error,
                CapSeverity::NeedsAck => LogLevel::Warn,
                CapSeverity::Info => LogLevel::Info,
            };
            ctx.log(lvl, "caps", i.render());
        }
        let blockers = wr.blockers();
        if !blockers.is_empty() {
            return refuse(
                "caps",
                blockers.iter().map(|i| i.render()).collect::<Vec<_>>().join("; "),
            );
        }
        let acks = wr.needs_ack();
        if !acks.is_empty() && !accept_caps {
            return refuse(
                "caps",
                format!(
                    "this apply degrades on capabilities the backends lack — rerun with --accept-caps to consent:\n  {}",
                    acks.iter().map(|i| i.render()).collect::<Vec<_>>().join("\n  ")
                ),
            );
        }
    }
    let verdict = crate::pipeline::guard::run_all_vfs(
        ops,
        &sv,
        &tv,
        plan.header.source_entries,
        plan.header.target_entries,
        &job.guards(acknowledged),
    );
    if !verdict.report("preflight") {
        for b in &verdict.blockers {
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::foundation::time::now_ms(),
                path: String::new(),
                action: "preflight".into(),
                side: "target".into(),
                message: b.clone(),
            });
        }
        return ApplyOutcome { done: 0, skipped: ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false };
    }
    let ap = apply::apply_vfs(ops, &sv, &tv, &job.apply_opts(trash, verbose), ctx);
    // A cancelled run does not refresh the archive: the user asked to "stop now", and re-reporting conflicts next round is safe anyway
    if ap.errors == 0 && !ap.cancelled && job.mode == "sync" {
        refresh_archive_with(job, plan, ctx);
    }
    let out = ApplyOutcome { cancelled: ctx.ctl.cancelled(), ..ap };
    ctx.sink.emit(ProgressEvent::Summary {
        ts_ms: crate::foundation::time::now_ms(),
        done: out.done,
        skipped: out.skipped,
        errors: out.errors,
        bytes_done: out.bytes_copied,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        paused_ms: ctx.ctl.paused_total_ms(),
        cancelled: out.cancelled,
    });
    out
}

/// End-to-end run for local/mounted-disk jobs (the body of the original CLI run). Returns (done, skipped, errors, conflicts).
pub fn run_local_job(name: &str, job: &Job, do_apply: bool, verbose: bool, acknowledged: bool, accept_caps: bool) -> std::io::Result<(u64, u64, u64, u64)> {
    // 1:N (the original requirement): one source → each target compared and executed independently.
    // One plan and one run log per target; source-side hashing is absorbed by the cache (in the fast tier, near-zero reads from the second target on).
    job.validate_multi_target().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let targets = job.target_list();
    let multi = targets.len() > 1;
    let mut tot = (0u64, 0u64, 0u64, 0u64);
    for (i, t) in targets.iter().enumerate() {
        let jt = job.for_target(t);
        let label = if multi { format!("{name}[{}/{} → {t}]", i + 1, targets.len()) } else { name.to_string() };
        let r = run_local_single(&label, &jt, do_apply, verbose, acknowledged, accept_caps)?;
        tot.0 += r.0;
        tot.1 += r.1;
        tot.2 += r.2;
        tot.3 += r.3;
    }
    Ok(tot)
}

pub fn run_local_single(name: &str, job: &Job, do_apply: bool, verbose: bool, acknowledged: bool, accept_caps: bool) -> std::io::Result<(u64, u64, u64, u64)> {
    let plan = compare_job_detailed(job, &crate::obs::progress::RunCtx::null(), accept_caps)?.plan;
    crate::log_info!("run", "[{name}] {} op(s), {} conflict(s)", plan.header.op_count, plan.header.conflict_count);
    for op in &plan.ops {
        println!("{}", serde_json::to_string(op)?);
    }
    if !do_apply {
        println!("dry-run (rerun with --apply)");
        return Ok((0, plan.ops.len() as u64, 0, plan.header.conflict_count));
    }
    let ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    // M4: the CLI's apply leaves a run log too (desktop records its own at the shell layer)
    let t0 = std::time::Instant::now();
    let rec = crate::obs::runlog::Recorder::start(name, "apply", &crate::obs::progress::RunCtx::null(), &ops);
    let out = apply_job_guarded_with(job, &plan, &ops, None, verbose, acknowledged, accept_caps, &rec.ctx);
    rec.finish(&out, t0.elapsed().as_millis() as u64);
    Ok((out.done, out.skipped, out.errors, plan.header.conflict_count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::memory::MemVfs;
    use crate::fs::vfs::Vfs;
    use crate::obs::progress::RunCtx;
    use std::sync::Arc;

    /// The generic VFS lane end to end: `as_local()` is None on both sides, so this drives
    /// `scan_vfs` rather than the walkdir fast path, then compares and plans.
    #[test]
    fn vfs_lane_compares_and_classifies_every_drift() {
        let sv = MemVfs::new("cmp-src");
        let tv = MemVfs::new("cmp-tgt");
        // byte-identical on both sides — must produce no op at all
        sv.seed_file("a/same.bin", 10_000, 1_000_000);
        tv.seed_file("a/same.bin", 10_000, 1_000_000);
        // source-only -> copy; target-only -> delete (mirror)
        sv.seed_file("a/new.bin", 5_000, 1_000_000);
        tv.seed_file("a/gone.bin", 7_000, 1_000_000);
        // same path, different size -> update
        sv.seed_file("a/changed.bin", 9_000, 2_000_000);
        tv.seed_file("a/changed.bin", 8_000, 1_000_000);
        // an excluded directory on both sides must be pruned AND counted
        sv.seed_file("skipme/x.bin", 100, 0);
        tv.seed_file("skipme/x.bin", 100, 0);

        let mut j = Job::default();
        j.mode = "mirror".into();
        j.rigor = "standard".into();
        j.exclude = vec!["skipme/".into()];
        let (sv, tv) = (Arc::new(sv) as Arc<dyn Vfs>, Arc::new(tv) as Arc<dyn Vfs>);
        let out = compare_resolved(&j, &sv, &tv, &RunCtx::null(), false).unwrap();

        assert_eq!(out.source.header.excluded_dirs, 1, "a pruned subtree must be counted, never silent");
        assert_eq!(out.target.header.excluded_dirs, 1);
        assert!(out.source.header.vfs.is_some(), "a VFS root's snapshot must carry its self-description");
        assert!(
            !out.source.entries.iter().any(|e| e.path.starts_with("skipme")),
            "pruned content must not enter the table"
        );

        let mut kinds: Vec<(String, String)> = out
            .plan
            .ops
            .iter()
            .map(|o| (format!("{:?}", o.action).to_lowercase(), o.path.clone()))
            .collect();
        kinds.sort();
        assert_eq!(
            kinds,
            vec![
                ("copy".to_string(), "a/new.bin".to_string()),
                ("delete".to_string(), "a/gone.bin".to_string()),
                ("update".to_string(), "a/changed.bin".to_string()),
            ],
            "same.bin must compare equal; the three drifts must each classify"
        );
    }

    /// A backend that cannot serve ranged reads degrades the sampled evidence tier. That must
    /// cost an explicit consent, and the consented degradation must ride on the snapshot.
    #[test]
    fn degraded_caps_demand_consent_and_land_on_the_table() {
        let sv = MemVfs::new("ack-src");
        let tv = MemVfs::new("ack-tgt").without(|c| c.ranged_read = crate::fs::vfs::Support::No);
        // Big enough that the sampled tier would sample, identical on both sides
        sv.seed_file("big.bin", 5 * 1024 * 1024, 1_000);
        tv.seed_file("big.bin", 5 * 1024 * 1024, 1_000);
        let mut j = Job::default();
        j.mode = "mirror".into();
        j.rigor = "fast".into(); // the sampled tier
        let (sv, tv) = (Arc::new(sv) as Arc<dyn Vfs>, Arc::new(tv) as Arc<dyn Vfs>);

        let e = match compare_resolved(&j, &sv, &tv, &RunCtx::null(), false) {
            Err(e) => e,
            Ok(_) => panic!("a degraded run must refuse without consent"),
        };
        assert!(e.to_string().contains("--accept-caps"), "{e}");

        // With consent: BOTH sides upgrade to full — a one-sided upgrade would make the
        // identical file look different — and the plan stays empty.
        let out = compare_resolved(&j, &sv, &tv, &RunCtx::null(), true).unwrap();
        assert_eq!(out.source.header.vfs.as_ref().unwrap().evidence_effective, "full");
        assert_eq!(out.target.header.vfs.as_ref().unwrap().evidence_effective, "full");
        assert!(
            !out.target.header.vfs.as_ref().unwrap().degraded.is_empty(),
            "the consented degradation must ride on the snapshot"
        );
        assert_eq!(out.plan.ops.len(), 0, "identical content must not produce ops after the joint upgrade");
    }

    /// An unknown scheme is a hard error at resolution, never a silent local path.
    #[test]
    fn resolve_refuses_an_unknown_scheme() {
        let e = match resolve_root("sfpt://typo/data") {
            Err(e) => e,
            Ok(_) => panic!("an unknown scheme must not resolve"),
        };
        assert!(e.to_string().contains("unknown scheme"), "{e}");
    }
}
