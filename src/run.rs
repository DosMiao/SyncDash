//! Job pipeline: scan both sides → compare (sync brings the archive along automatically) → apply → refresh the archive on success.
//! The CLI's `run` and the GUI share this one implementation.

use crate::model::plan::{Action, Op, Plan};
use crate::job::Job;
use crate::model::table::Snapshot;
use crate::pipeline::{apply, compare, scan};
use std::path::Path;

/// Rigor level → scan options. A monotone ladder: each tier = **more actually read this round**,
/// and the meaning of "identical ✓" climbs from "metadata measured" to "every byte measured".
/// quick    reads 0 bytes (size+mtime)
/// fast     really reads only the sampling window of changed faces (the cache covers unchanged ones)
/// standard really reads every file's sampling window this round (no cache — not memory, a measurement taken now)
/// paranoid really reads every byte of every file this round
pub fn scan_opts(job: &Job) -> scan::ScanOptions {
    let filter = crate::pipeline::filter::PathFilter::build_full(&job.include, &job.exclude, &job.deletable);
    let r = job.rigor_resolved();
    scan::ScanOptions { hash: r.hash, sampled: r.sampled, use_cache: r.use_cache, symlinks_direct: job.symlinks == "direct", filter }
}

/// Refresh the archive after a successful sync: rescan source, drop conflicted paths (a conflict keeps being reported, never silently arbitrated).
/// v0.9 M1: make the Refresh phase visible — the archive rescan is a long phase that is completely invisible today, so wire it to the event stream and cancellation.
/// Being cancelled only means conflicts get re-reported next round — safe.
fn refresh_archive_with(job: &Job, plan: &Plan, ctx: &crate::obs::progress::RunCtx) {
    let Some(arch_path) = &job.archive else {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "run",
            "hint: sync job without `archive` — add one so deletions/moves can be attributed next time",
        );
        return;
    };
    let conflicted: std::collections::HashSet<&str> = plan
        .ops
        .iter()
        .filter(|o| o.action == Action::Conflict)
        .map(|o| o.path.as_str())
        .collect();
    let opt = scan_opts(job);
    // The previous-generation archive: every row of the new table pushes the old hash onto the prev chain, so that
    // "one generation behind" can be told apart from "concurrent modification" (P1-3, see compare::generation_of)
    let previous = if arch_path.is_file() { Snapshot::load(arch_path).ok() } else { None };
    let Ok(svr) = resolve_root(&job.source) else { return };
    if let Ok(mut snap) = scan::scan_root(&svr, &opt, ctx, crate::model::event::Phase::Refresh) {
        snap.header.kind = "archive".into();
        snap.entries.retain(|e| !conflicted.contains(e.path.as_str()));
        if let Some(prev) = &previous {
            crate::model::table::roll_generations(&mut snap.entries, &prev.entries);
        }
        if let Some(dir) = arch_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(f) = std::fs::File::create(arch_path) {
            let mut w = std::io::BufWriter::new(f);
            if snap.write_to(&mut w).is_ok() {
                ctx.log(
                    crate::model::event::LogLevel::Info,
                    "run",
                    format!("archive refreshed: {}", arch_path.display()),
                );
            }
        }
    }
}

/// Resolve one root phrase to something the engine's local lanes can touch.
/// Local paths pass through untouched. A backend that *translates* to a local path
/// (smb:// → UNC / mount point) connects first — that is where mount orchestration
/// lives — and hands back the translated path. A genuinely remote backend has no
/// local path; until its engine lane exists (scan M3 / apply M4) that is a loud
/// error naming the milestone, never a silent fallback.
/// Open + connect a root phrase to a live backend (a plain local path resolves to LocalVfs).
pub fn resolve_root(s: &str) -> std::io::Result<std::sync::Arc<dyn crate::fs::vfs::Vfs>> {
    let v = crate::fs::vfs::open(s, &crate::fs::vfs::cred::default_provider())?;
    v.connect().map_err(std::io::Error::from)?;
    Ok(v)
}

pub fn compare_job(job: &Job) -> std::io::Result<Plan> {
    compare_job_with(job, &crate::obs::progress::RunCtx::null())
}

/// One connected backend's capability sheet, rendered for `syncdash caps`.
pub fn describe_root(phrase: &str) -> std::io::Result<String> {
    use std::fmt::Write as _;
    let v = resolve_root(phrase)?;
    let c = v.caps();
    let mut out = String::new();
    let _ = writeln!(out, "root:      {}", v.display());
    let _ = writeln!(out, "identity:  {}", v.identity());
    if let Some(info) = v.server_info() {
        let _ = writeln!(out, "server:    {info}");
    }
    let _ = writeln!(out, "protocol:  {}", c.protocol);
    let _ = writeln!(out, "local lane: {}", if v.as_local().is_some() { "yes (fast path)" } else { "no (generic VFS lane)" });
    let _ = writeln!(out, "mtime precision: {} ms", c.mtime_precision_ms);
    let _ = writeln!(out, "name rules: {}{}", c.name_rules.as_str(), match c.name_rules {
        crate::fs::vfs::NameRules::Windows =>
            "  (no <>:\"|?*, no trailing dot or space, no reserved device names — such paths are refused at plan time)",
        crate::fs::vfs::NameRules::Posix => "",
        crate::fs::vfs::NameRules::Unknown =>
            "  (this protocol does not reveal the server's OS — risky names are warned about, not refused)",
    });
    let cap = |s: crate::fs::vfs::Support| match s {
        crate::fs::vfs::Support::Yes => "yes",
        crate::fs::vfs::Support::No => "NO",
        crate::fs::vfs::Support::Unknown => "unknown",
    };
    let _ = writeln!(out, "set_mtime {}  fsync {}  rename_overwrite {}  ranged_read {}  write_at {}",
        cap(c.set_mtime), cap(c.fsync), cap(c.rename_overwrite), cap(c.ranged_read), cap(c.write_at));
    let _ = writeln!(out, "unix_mode {}  symlink {}  file_id {}  free_space {}  read_back {}",
        cap(c.unix_mode), cap(c.symlink), cap(c.file_id), cap(c.free_space), cap(c.read_back));
    let _ = write!(out, "parallel streams: up to {}", c.max_parallel_streams);
    Ok(out)
}

/// The full output of a comparison: the plan plus both snapshots.
/// The desktop shell needs the snapshots to compute the "evidence layer" (both sides' size/mtime, identical items); the CLI only wants the plan.
pub struct CompareOutcome {
    pub plan: Plan,
    pub source: Snapshot,
    pub target: Snapshot,
}

/// v0.9 M1: the end-to-end comparison with an event stream and cancellation. This function replaces the second
/// pipeline src-tauri had inlined just to emit events (undoing the two-copy drift).
pub fn compare_job_with(job: &Job, ctx: &crate::obs::progress::RunCtx) -> std::io::Result<Plan> {
    compare_job_detailed(job, ctx, false).map(|o| o.plan)
}

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
    crate::pipeline::guard::check_root_vfs("source", sv, job.require_marker, &mut v);
    crate::pipeline::guard::check_root_vfs("target", tv, job.require_marker, &mut v);
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
    let caps_report = crate::pipeline::guard::cap_report_read(&q, &sv.caps(), &tv.caps());
    {
        use crate::model::event::LogLevel;
        use crate::pipeline::guard::CapSeverity;
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
        use crate::pipeline::guard::CapSeverity;
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

/// Execute the selected ops; on complete success in sync mode, refresh the archive (conflicted paths are dropped from it, so the conflict is reported again next time).
pub fn apply_job(job: &Job, plan: &Plan, ops: &[Op], trash: Option<std::path::PathBuf>, verbose: bool) -> (u64, u64, u64) {
    apply_job_guarded(job, plan, ops, trash, verbose, false, false)
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

/// `acknowledged` = the user passed --i-know explicitly; it only allows the "plan health check" gates through.
/// A missing marker and insufficient disk space always block (those are environment problems, not judgment calls).
pub fn apply_job_guarded(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
) -> (u64, u64, u64) {
    apply_job_guarded_with(job, plan, ops, trash, verbose, acknowledged, accept_caps, &crate::obs::progress::RunCtx::null()).into_tuple()
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
        use crate::pipeline::guard::CapSeverity;
        let q = job.write_caps_query(sv.as_local().is_some(), tv.as_local().is_some());
        let wr = crate::pipeline::guard::cap_report_write(&q, ops, &sv.caps(), &tv.caps());
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

fn run_local_single(name: &str, job: &Job, do_apply: bool, verbose: bool, acknowledged: bool, accept_caps: bool) -> std::io::Result<(u64, u64, u64, u64)> {
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

/// Remote pipeline (the v0.6 end-to-end over ssh): ssh probe → remote-local scan (table collected from stdout) → local scan → compare
/// → pack the target side and ship it over ssh to apply-pack → write the source side straight through the mounted path → refresh the archive on a successful sync.
pub fn run_remote_job(name: &str, job: &Job, do_apply: bool, verbose: bool, acknowledged: bool) -> std::io::Result<(u64, u64, u64, u64)> {
    run_remote_job_with(name, job, do_apply, verbose, acknowledged, &crate::obs::progress::RunCtx::null())
}

/// v0.9 M1/M3: the remote pipeline = a compare stage plus an apply stage (desktop calls each over its own IPC round; the CLI runs both end-to-end here).
/// PhaseStart at each stage boundary, cooperation points between stages, a Summary terminal state; byte-level counting inside the
/// ssh transfer and kill-on-cancel are explicitly deferred (M1 step 8).
pub fn run_remote_job_with(
    name: &str,
    job: &Job,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<(u64, u64, u64, u64)> {
    let plan = match compare_remote_job_with(name, job, ctx) {
        Ok(p) => p,
        Err(e) => {
            // Cancelled in the compare stage: the terminal state must still be visible (the desktop closes out on Summary)
            if crate::obs::progress::is_cancelled(&e) {
                emit_cancel_summary(ctx, std::time::Instant::now());
            }
            return Err(e);
        }
    };
    ctx.log(
        crate::model::event::LogLevel::Info,
        "run",
        format!(
            "[{name}] {} op(s), {} conflict(s)  (remote pipeline via ssh)",
            plan.header.op_count, plan.header.conflict_count
        ),
    );
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
    let t0 = std::time::Instant::now();
    let rec = crate::obs::runlog::Recorder::start(name, "remote-apply", ctx, &ops);
    let out = apply_remote_job_with(name, job, &plan, &ops, verbose, acknowledged, &rec.ctx)?;
    rec.finish(&out, t0.elapsed().as_millis() as u64);
    Ok((out.done, out.skipped, out.errors, plan.header.conflict_count))
}

fn emit_cancel_summary(ctx: &crate::obs::progress::RunCtx, t0: std::time::Instant) {
    ctx.sink.emit(crate::model::event::ProgressEvent::Summary {
        ts_ms: crate::foundation::time::now_ms(),
        done: 0,
        skipped: 0,
        errors: 0,
        bytes_done: 0,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        paused_ms: ctx.ctl.paused_total_ms(),
        cancelled: true,
    });
}

/// Remote connection parameters (the product of a probe). The desktop's compare and apply are two independent IPC rounds
/// with no connection kept in between — the apply stage probes again (one ssh round trip, which doubles as a reachability preflight).
pub struct RemoteLink {
    pub host: String,
    pub exe: String,
    pub rroot: String,
    pub shell: crate::transfer::remote::RemoteShell,
}

/// Stage 1: probe reachability + schema agreement + the remote OS (which decides the shell dialect).
///
/// It takes `ctx` for the sake of the schema-mismatch warning: that one has to reach the UI **during compare**.
/// Going through the macro and the global registry, no sink is installed during compare (only apply starts a Recorder),
/// so this line in particular would fall back to stderr — which in a windowed desktop build is the same as saying nothing.
pub fn probe_remote(name: &str, job: &Job, ctx: &crate::obs::progress::RunCtx) -> std::io::Result<RemoteLink> {
    let host = job
        .remote_host
        .as_deref()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "remote_host not set"))?;
    let exe = job.remote_exe.as_deref().unwrap_or("syncdash");
    let rroot = job
        .remote_root
        .as_deref()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "remote_root required for remote jobs"))?;
    let probe = crate::transfer::remote::ssh_capture(host, &format!("{exe} probe"))?;
    let pv: serde_json::Value = serde_json::from_slice(&probe)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad probe output: {e}")))?;
    if pv["schema"].as_u64() != Some(crate::model::table::SCHEMA as u64) {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "remote",
            format!(
                "[{name}] warning: remote schema {} != local {} — rebuild the remote binary",
                pv["schema"],
                crate::model::table::SCHEMA
            ),
        );
    }
    let remote_os = pv["os"].as_str().unwrap_or("").to_string();
    ctx.log(
        crate::model::event::LogLevel::Info,
        "remote",
        format!("[{name}] remote {}: {} {}", host, remote_os, pv["arch"].as_str().unwrap_or("?")),
    );
    Ok(RemoteLink {
        host: host.to_string(),
        exe: exe.to_string(),
        rroot: rroot.to_string(),
        shell: crate::transfer::remote::RemoteShell::from_os(&remote_os),
    })
}

/// v0.9 M3: the **compare stage** of a remote job — the desktop's `compare_job` routes remote jobs straight here
/// instead of silently dropping into the local pipeline (which would pull the data over UNC and rehash it: an order of magnitude slower, and semantically wrong).
pub fn compare_remote_job_with(name: &str, job: &Job, ctx: &crate::obs::progress::RunCtx) -> std::io::Result<Plan> {
    compare_remote_job_detailed(name, job, ctx).map(|o| o.plan)
}

/// The same detailed variant for the remote pipeline: the remote snapshot is a complete table pulled back over ssh,
/// so the evidence layer (both sides' size/mtime, identical items) is just as computable here as for a local job.
pub fn compare_remote_job_detailed(
    name: &str,
    job: &Job,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<CompareOutcome> {
    use crate::model::event::Phase;
use crate::obs::progress::PhaseProgress;
    let link = probe_remote(name, job, ctx)?;

    // 2) Remote scan (hashing on the remote's own disk — far faster than pulling the data over UNC)
    // The remote is passed the **resolved** knobs explicitly (a preset name isn't enough — details may have overridden it)
    let rr = job.rigor_resolved();
    let mut scan_args: Vec<String> = vec![
        "scan".into(),
        link.rroot.clone(),
        "--evidence".into(),
        (if !rr.hash { "none" } else if rr.sampled { "sampled" } else { "full" }).into(),
        "--cache".into(),
        (if rr.use_cache { "on" } else { "off" }).into(),
    ];
    // `--junk none`: the job's `exclude` already carries every junk pattern it wants, so letting the
    // remote add its own CLI default on top would make the two sides filter differently — and a rule
    // that applies to only one root is the shape that gets a tree proposed for deletion.
    scan_args.push("--junk".into());
    scan_args.push("none".into());
    for ex in &job.exclude {
        scan_args.push("--exclude".into());
        scan_args.push(ex.clone());
    }
    if job.symlinks == "direct" {
        scan_args.push("--symlinks-direct".into());
    }
    ctx.checkpoint()?;
    // The remote scans on its own disk, so locally all we can show is "in progress" — totals zeroed, the label spells out who we are waiting on
    let _pp_rs = PhaseProgress::begin(ctx, Phase::ScanTarget, Some(format!("ssh:{} {}", link.host, link.rroot)), 0, 0);
    let table_bytes = crate::transfer::remote::ssh_capture(&link.host, &crate::transfer::remote::remote_cmd(link.shell, &link.exe, &scan_args))?;
    let t = Snapshot::from_reader(std::io::BufReader::new(&table_bytes[..]))?;

    // 3) Local scan + compare (the local source side goes through the mount-point gate too)
    let mut v = crate::pipeline::guard::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::pipeline::guard::check_root("source", job.source_path(), job.require_marker, &mut v);
    for w in &v.warnings {
        ctx.log(crate::model::event::LogLevel::Warn, "compare", format!("[{name}] warning: {w}"));
    }
    if !v.ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, v.blockers.join("; ")));
    }
    let s = scan::scan_ctx(job.source_path(), &scan_opts(job), ctx, Phase::ScanSource)?;
    let archive = match (&job.archive, job.mode.as_str()) {
        (Some(p), "sync") if p.is_file() => Some(Snapshot::load(p)?),
        _ => None,
    };
    let _pp_cmp = PhaseProgress::begin(
        ctx,
        Phase::Compare,
        Some(format!("{} × {} entries", s.header.entry_count, t.header.entry_count)),
        0,
        0,
    );
    let copts = job.compare_opts();
    let plan = compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &copts);
    Ok(CompareOutcome { plan, source: s, target: t })
}

/// The plan health check for a remote job (used by the desktop confirmation sheet): the deletion-share gate only —
/// disk space and the marker live on the remote machine; we cannot check them locally and must not pretend we did.
pub fn preflight_remote_job(job: &Job, plan: &Plan, ops: &[Op], acknowledged: bool) -> crate::pipeline::guard::Verdict {
    let g = job.guards(acknowledged);
    let st = crate::pipeline::guard::stat_plan(ops);
    let mut gv = crate::pipeline::guard::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::pipeline::guard::check_delete_ratio("target", &st.target, plan.header.target_entries, &g, &mut gv);
    crate::pipeline::guard::check_delete_ratio("source", &st.source, plan.header.source_entries, &g, &mut gv);
    gv
}

/// v0.9 M3: the **apply stage** of a remote job — `ops` is the subset the user finalised in the diff table (direction flips / check marks already applied).
/// Probe again → health check → pack the selection → ship the package over ssh → remote apply-pack → pull the source side back → refresh → Summary.
pub fn apply_remote_job_with(
    name: &str,
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<crate::obs::progress::ApplyOutcome> {
    let t0 = std::time::Instant::now();
    let r = apply_remote_inner(name, job, plan, ops, verbose, acknowledged, ctx, t0);
    if let Err(e) = &r {
        if crate::obs::progress::is_cancelled(e) {
            emit_cancel_summary(ctx, t0);
        }
    }
    r
}

#[allow(clippy::too_many_arguments)]
fn apply_remote_inner(
    name: &str,
    job: &Job,
    plan_full: &Plan,
    sel_ops: &[Op],
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::obs::progress::RunCtx,
    t0: std::time::Instant,
) -> std::io::Result<crate::obs::progress::ApplyOutcome> {
    use crate::model::plan::Side;
    use crate::model::event::{Phase, ProgressEvent};
use crate::obs::progress::{ApplyOutcome, PhaseProgress};

    // Plan health check: remote disk space is unknowable, but an accident like "delete most of the other side" can be caught locally
    let gv = preflight_remote_job(job, plan_full, sel_ops, acknowledged);
    if !gv.report(name) {
        for b in &gv.blockers {
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::foundation::time::now_ms(),
                path: String::new(),
                action: "preflight".into(),
                side: "target".into(),
                message: b.clone(),
            });
        }
        return Ok(ApplyOutcome { done: 0, skipped: sel_ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false });
    }

    let link = probe_remote(name, job, ctx)?;
    let (host, exe, rroot, shell) = (link.host.as_str(), link.exe.as_str(), link.rroot.as_str(), link.shell);
    // Packing and the pull-back only look at the finalised subset; the full plan is used only for the archive refresh (dropping conflicted paths needs all of it)
    let plan = Plan { header: plan_full.header.clone(), ops: sel_ops.to_vec() };

    let mut done = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;
    let mut bytes_done_total = 0u64;

    // 4) Target side: (for large updates, fetch the remote chunk table first so FastCDC delta can be used) pack → ship the package over ssh → remote apply-pack
    let has_target_ops = plan.ops.iter().any(|o| o.side == Side::Target && !matches!(o.action, Action::Conflict | Action::Note));
    if has_target_ops {
        let delta_rels: Vec<String> = plan
            .ops
            .iter()
            .filter(|o| {
                o.side == Side::Target
                    && o.action == Action::Update
                    && o.link.is_none()
                    && o.size.map(|s| s >= crate::model::chunk::DELTA_MIN_SIZE).unwrap_or(false)
            })
            .map(|o| o.path.clone())
            .collect();
        let remote_chunks = if delta_rels.is_empty() {
            None
        } else {
            let mut args: Vec<String> = vec!["chunks".into(), "--root".into(), rroot.to_string()];
            for r in &delta_rels {
                args.push("--file".into());
                args.push(r.clone());
            }
            match crate::transfer::remote::ssh_capture(host, &crate::transfer::remote::remote_cmd(shell, exe, &args)) {
                Ok(bytes) => {
                    let mut m = std::collections::HashMap::new();
                    for line in String::from_utf8_lossy(&bytes).lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if let Ok(fc) = serde_json::from_str::<crate::model::chunk::FileChunks>(line) {
                            m.insert(fc.rel.clone(), fc);
                        }
                    }
                    ctx.log(
                        crate::model::event::LogLevel::Info,
                        "delta",
                        format!("[{name}] delta: got chunk tables for {} large file(s)", m.len()),
                    );
                    Some(m)
                }
                Err(e) => {
                    ctx.log(
                        crate::model::event::LogLevel::Warn,
                        "delta",
                        format!("[{name}] delta disabled (chunk request failed: {e})"),
                    );
                    None
                }
            }
        };
        ctx.checkpoint()?;
        let pp_pack = PhaseProgress::begin(ctx, Phase::Pack, Some("packing target-side content".into()), 0, 0);
        let tmp = std::env::temp_dir().join(format!("syncdash-remote-{}.tar", crate::foundation::time::now_ms()));
        let sum = crate::transfer::pack::pack(&plan, job.source_path(), &tmp, remote_chunks.as_ref())?;
        pp_pack.set_totals(sum.ops, sum.bytes);
        if sum.delta_saved > 0 {
            ctx.log(
                crate::model::event::LogLevel::Info,
                "pack",
                format!("[{name}] packed {} B, delta saved {} B", sum.bytes, sum.delta_saved),
            );
        }
        let rpkg = if shell == crate::transfer::remote::RemoteShell::PowerShell {
            format!("syncdash-{}.tar", crate::foundation::time::now_ms()) // relative path → the remote home directory
        } else {
            format!("/tmp/syncdash-{}.tar", crate::foundation::time::now_ms())
        };
        ctx.checkpoint()?;
        let tar_len = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        let pp_ship = PhaseProgress::begin(ctx, Phase::Ship, Some(format!("→ ssh:{host}")), 1, tar_len);
        let recv_cmd = crate::transfer::remote::remote_cmd(shell, exe, &["recv".into(), rpkg.clone()]);
        let ship = crate::transfer::remote::ssh_run_with_stdin(host, &recv_cmd, &tmp);
        let _ = std::fs::remove_file(&tmp);
        ship?;
        pp_ship.add_bytes(tar_len, &rpkg);
        pp_ship.item_done(&rpkg);
        bytes_done_total += sum.bytes;

        ctx.checkpoint()?;
        let _pp_ra = PhaseProgress::begin(ctx, Phase::Apply, Some(format!("ssh:{host} apply-pack")), sum.ops, 0);
        let mut ap_args: Vec<String> = vec!["apply-pack".into(), rpkg.clone(), "--apply".into(), "--remove-pkg".into()];
        if job.versioning {
            ap_args.push("--versioning".into());
        }
        if verbose {
            ap_args.push("-v".into());
        }
        let ok = crate::transfer::remote::ssh_run(host, &crate::transfer::remote::remote_cmd(shell, exe, &ap_args))?;
        if ok {
            done += sum.ops;
        } else {
            errors += 1;
            ctx.log(crate::model::event::LogLevel::Error, "remote", format!("[{name}] remote apply-pack reported failure"));
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::foundation::time::now_ms(),
                path: rpkg.clone(),
                action: "apply-pack".into(),
                side: "target".into(),
                message: "remote apply-pack reported failure".into(),
            });
        }
    }

    // 5) Source side (sync's pull-back direction): read the remote content straight through the mounted path; if it is unreachable, skip and say why
    let src_ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| o.side == Side::Source && !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    if !src_ops.is_empty() {
        if job.target_path().is_dir() {
            let out = crate::pipeline::apply::apply_with(
                &src_ops,
                job.source_path(),
                job.target_path(),
                &job.apply_opts(None, verbose),
                ctx,
            );
            done += out.done;
            skipped += out.skipped;
            errors += out.errors;
            bytes_done_total += out.bytes_copied;
        } else {
            skipped += src_ops.len() as u64;
            ctx.log(
                crate::model::event::LogLevel::Warn,
                "remote",
                format!(
                    "[{name}] {} source-side op(s) skipped: mounted target '{}' not accessible (pull direction needs the SMB mount)",
                    src_ops.len(),
                    job.target
                ),
            );
        }
    }

    if errors == 0 && !ctx.ctl.cancelled() && job.mode == "sync" {
        refresh_archive_with(job, plan_full, ctx);
    }
    ctx.sink.emit(ProgressEvent::Summary {
        ts_ms: crate::foundation::time::now_ms(),
        done,
        skipped,
        errors,
        bytes_done: bytes_done_total,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        paused_ms: ctx.ctl.paused_total_ms(),
        cancelled: ctx.ctl.cancelled(),
    });
    Ok(ApplyOutcome {
        done,
        skipped,
        errors,
        bytes_copied: bytes_done_total,
        cancelled: ctx.ctl.cancelled(),
    })
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
