//! Jobs that execute in this process.
//!
//! "Local" is about the transport, not the roots: an `sftp://` target still runs here, because
//! this process does the reading and writing. What makes a job *not* local is a peer that runs
//! syncdash itself — see `remote`.

use crate::job::Job;
use crate::model::plan::{Action, Op, Plan};
use crate::model::table::Snapshot;
use crate::pipeline::{apply, compare, scan};

use super::archive::refresh_archive_with;
use super::roots::resolve_root;
use super::CompareOutcome;

fn schedule_scans<S, T, FS, FT>(
    same_local_volume: bool,
    scan_source: FS,
    scan_target: FT,
) -> std::io::Result<(S, T)>
where
    S: Send,
    T: Send,
    FS: FnOnce() -> std::io::Result<S> + Send,
    FT: FnOnce() -> std::io::Result<T> + Send,
{
    if same_local_volume {
        let source = scan_source()?;
        let target = scan_target()?;
        Ok((source, target))
    } else {
        std::thread::scope(|scope| {
            let source = scope.spawn(scan_source);
            let target = scope.spawn(scan_target);
            Ok((source.join().unwrap()?, target.join().unwrap()?))
        })
    }
}

/// The same pipeline, but it also hands back both snapshots (throwing them away would force the UI to scan all over again)
/// `accept_caps` = the user consented (--accept-caps / a ticked confirmation box) to the
/// NeedsAck lines of the capability report. Without consent a degraded run refuses to start.
pub fn compare_job_detailed(
    job: &Job,
    ctx: &crate::obs::progress::RunCtx,
    accept_caps: bool,
) -> std::io::Result<CompareOutcome> {
    compare_job_detailed_with_consent(
        job,
        ctx,
        &crate::pipeline::guard::caps::CapabilityConsent::explicit_cli(accept_caps),
    )
}

pub fn compare_job_detailed_with_consent(
    job: &Job,
    ctx: &crate::obs::progress::RunCtx,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
) -> std::io::Result<CompareOutcome> {
    // The single pipeline handles a single target only: multi-target jobs are derived through for_target first (the CLI run loop / the desktop target picker)
    job.validate()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
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
    compare_resolved_with_consent(job, &sv, &tv, ctx, consent)
}

pub fn compare_capabilities(job: &Job) -> std::io::Result<crate::pipeline::guard::caps::CapReport> {
    job.validate()
        .map_err(|reason| std::io::Error::new(std::io::ErrorKind::InvalidInput, reason))?;
    let source = resolve_root(&job.source)?;
    let target = resolve_root(&job.target)?;
    let window_ms = job
        .compare_opts()
        .mtime_window_ms
        .max(source.caps().mtime_precision_ms as i64)
        .max(target.caps().mtime_precision_ms as i64);
    Ok(read_capabilities_resolved(job, &source, &target, window_ms))
}

fn read_capabilities_resolved(
    job: &Job,
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    window_ms: i64,
) -> crate::pipeline::guard::caps::CapReport {
    let query = job.read_caps_query(
        window_ms,
        source.as_local().is_some(),
        target.as_local().is_some(),
    );
    crate::pipeline::guard::caps::cap_report_read(&query, &source.caps(), &target.caps())
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
    compare_resolved_with_consent(
        job,
        sv,
        tv,
        ctx,
        &crate::pipeline::guard::caps::CapabilityConsent::explicit_cli(accept_caps),
    )
}

pub fn compare_resolved_with_consent(
    job: &Job,
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    ctx: &crate::obs::progress::RunCtx,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
) -> std::io::Result<CompareOutcome> {
    use crate::model::event::Phase;
    let opt = super::effective_scan_opts(job, sv, tv);
    // P0-2: root reachability + mount-point marker. When the share isn't mounted, target is often an empty directory,
    // and comparing as usual yields a plan that either "wipes the other side" or "re-sends everything".
    let mut v = crate::pipeline::guard::Verdict {
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    crate::pipeline::guard::roots::check_root_vfs("source", sv, job.require_marker, &mut v);
    crate::pipeline::guard::roots::check_root_vfs("target", tv, job.require_marker, &mut v);
    for w in &v.warnings {
        // v0.10: Log{Warn} replaces the Error{action:"warning"} hack —
        // with a real level, a warning no longer has to masquerade as an "error that doesn't count"
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "compare",
            format!("warning: {w}"),
        );
    }
    if !v.ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            v.blockers.join("; "),
        ));
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
    let caps_report = read_capabilities_resolved(job, sv, tv, copts.mtime_window_ms);
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
                blockers
                    .iter()
                    .map(|i| i.render())
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
        let acks = caps_report.needs_ack();
        if !caps_report.consent_satisfied(
            crate::pipeline::guard::caps::CapabilityScope::CompareRead,
            consent,
        ) {
            let instruction = match consent {
                crate::pipeline::guard::caps::CapabilityConsent::ExactDigest(_) => {
                    "the capability report changed after it was reviewed — review Compare again"
                }
                _ => "rerun with --accept-caps to consent",
            };
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "this run degrades on capabilities the backends lack — {instruction}:\n  {}",
                    acks.iter()
                        .map(|i| i.render())
                        .collect::<Vec<_>>()
                        .join("\n  ")
                ),
            ));
        }
    }
    // Different volumes/links scan in parallel, so wall clock is approximately the slower side.
    // Two roots on the same mounted volume scan sequentially to avoid competing metadata walks;
    // this is a filesystem identity (`st_dev`/volume root), not a claim about the physical disk.
    let same_local_volume = match (sv.as_local(), tv.as_local()) {
        (Some(source), Some(target)) => crate::fs::vfs::local::same_device(source, target),
        _ => false,
    };
    let (mut s, mut t) = schedule_scans(
        same_local_volume,
        || scan::scan_root(sv, &opt, ctx, Phase::ScanSource),
        || scan::scan_root(tv, &opt, ctx, Phase::ScanTarget),
    )?;
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
        (Some(p), "sync") if p.is_file() => {
            let a = Snapshot::load(p)?;
            // An archive is only usable against digests of its own tier: `~` sampled values are
            // prefixed so they can never equal a full hash, so comparing across tiers would call
            // every large file changed and turn a plain deletion into a delete-versus-edit
            // conflict that no `on_conflict` policy can resolve. Refusing the archive drops sync
            // into the documented no-archive safe mode — it fills both ways and reports rather
            // than deletes — which is a loss of attribution, not of data.
            let want = super::evidence_label(&opt);
            match a.header.vfs.as_ref().map(|v| v.evidence_effective.as_str()) {
                Some(had) if had != want => {
                    ctx.log(
                        crate::model::event::LogLevel::Warn,
                        "compare",
                        format!(
                            "archive was written with {had} evidence but this run compares at {want} — \
                             the two cannot be matched, so it is being ignored for this run. Sync \
                             falls back to safe mode (fills both ways, reports differences, deletes \
                             nothing). The next successful run rewrites it at {want}."
                        ),
                    );
                    None
                }
                _ => Some(a),
            }
        }
        _ => None,
    };
    // compare itself is sub-second CPU work: report the phase boundary only, no internal counting
    let pp = crate::obs::progress::PhaseProgress::begin(
        ctx,
        Phase::Compare,
        Some(format!(
            "{} × {} entries",
            s.header.entry_count, t.header.entry_count
        )),
        0,
        0,
    );
    let plan = compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &copts);
    // Disagreement escalation: in the sampled evidence tier, a file whose digests match but whose mtimes differ by >2s may not simply be ruled identical (the knob can turn this off)
    let rr = job.rigor_resolved();
    let plan = if rr.sampled && rr.escalate {
        escalate_sampled_disagreements(job, plan, &mut s, &mut t, ctx, sv, tv, &pp)?
    } else {
        plan
    };
    pp.finish()?;
    Ok(CompareOutcome {
        plan,
        source: s,
        target: t,
        compare_options: copts,
    })
}

/// The escalation rule: when the two signals fight (the sampled digest says "identical", mtime says "touched"), believe neither silently —
/// **escalate that file to a full hash on both sides** and rule again. This shrinks the blind spot of fast/balanced/standard from
/// "any change outside the sampling window" to "outside the sampling window *and* timestamp-preserving" (≈ the timestomp case).
/// The escalation set is naturally tiny (near zero on a normal tree), so reading each one in full
/// costs next to nothing. Reads stay on the VFS surface so local and protocol roots get the same
/// safety rule and the same classified failures.
fn escalate_sampled_disagreements(
    job: &Job,
    mut plan: Plan,
    s: &mut Snapshot,
    t: &mut Snapshot,
    ctx: &crate::obs::progress::RunCtx,
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    pp: &crate::obs::progress::PhaseProgress<'_>,
) -> std::io::Result<Plan> {
    use crate::model::event::LogLevel;
    use crate::model::plan::Side;
    use crate::model::table::EntryKind;
    // The same equality window used by the comparison above. Protocol roots such as FTP can be
    // coarser than the job default, and that precision must not manufacture escalation work.
    let slack_ms = job
        .compare_opts()
        .mtime_window_ms
        .max(source.caps().mtime_precision_ms as i64)
        .max(target.caps().mtime_precision_ms as i64);

    const MAX_READ_CHUNK: usize = 8 * 1024 * 1024;

    fn full_hash(
        vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
        side: &str,
        rel: &str,
        pp: &crate::obs::progress::PhaseProgress<'_>,
    ) -> std::io::Result<String> {
        use std::io::Read;
        pp.checkpoint()?;
        let mut stream = vfs.open_read(rel).map_err(|error| {
            let error: std::io::Error = error.into();
            std::io::Error::new(
                error.kind(),
                format!("cannot fully verify {side} '{rel}': open failed: {error}"),
            )
        })?;
        let mut h = blake3::Hasher::new();
        let mut buf = vec![0u8; stream.block_size().clamp(64 * 1024, MAX_READ_CHUNK)];
        loop {
            pp.checkpoint()?;
            let n = stream.read(&mut buf).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("cannot fully verify {side} '{rel}': read failed: {error}"),
                )
            })?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            pp.add_bytes(n as u64, rel);
        }
        Ok(h.finalize().to_hex().to_string())
    }
    let target_by_path: std::collections::HashMap<&str, (usize, &crate::model::table::Entry)> = t
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.kind == EntryKind::File)
        .map(|(index, entry)| (entry.path.as_str(), (index, entry)))
        .collect();
    let suspects: Vec<(
        usize,
        usize,
        crate::model::table::Entry,
        crate::model::table::Entry,
    )> = s
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.kind == EntryKind::File)
        .filter_map(|(source_index, source_entry)| {
            target_by_path
                .get(source_entry.path.as_str())
                .map(|(target_index, target_entry)| {
                    (
                        source_index,
                        *target_index,
                        source_entry.clone(),
                        (*target_entry).clone(),
                    )
                })
        })
        .filter(|(_, _, source_entry, target_entry)| {
            match (&source_entry.hash, &target_entry.hash) {
                (Some(a), Some(b)) => {
                    a.starts_with('~')
                        && a == b
                        && (source_entry.mtime_ms - target_entry.mtime_ms).abs() > slack_ms
                }
                _ => false,
            }
        })
        .collect();
    if suspects.is_empty() {
        return Ok(plan);
    }
    let bytes_total = suspects.iter().fold(0u64, |total, (_, _, source, target)| {
        total
            .saturating_add(source.size)
            .saturating_add(target.size)
    });
    pp.set_totals(suspects.len() as u64, bytes_total);
    ctx.log(
        LogLevel::Info,
        "compare",
        format!(
            "escalation: {} file(s) with equal sampled digests but mtimes outside the comparison window — re-verifying both sides in full",
            suspects.len()
        ),
    );
    // One VFS object can represent a single protocol session (FTP is the canonical case), so
    // escalation must not open multiple suspects concurrently behind that backend's back. The
    // set is normally empty or one item; sequential reads are the safe and realistic schedule.
    let escalated: Vec<(usize, usize, String, String, Option<Op>)> = suspects
        .iter()
        .map(
            |(source_index, target_index, se, te)| -> std::io::Result<(
                usize,
                usize,
                String,
                String,
                Option<Op>,
            )> {
                let hs = full_hash(source, "source", &se.path, pp)?;
                let ht = full_hash(target, "target", &te.path, pp)?;
                let op = if hs == ht {
                    None
                } else {
                    let reason =
                        "escalated: sampled digests equal, mtime differs, full hashes differ";
                    match job.mode.as_str() {
                        // mirror: source wins unconditionally
                        "mirror" => Some(Op {
                            side: Side::Target,
                            action: Action::Update,
                            path: se.path.clone(),
                            from: None,
                            size: Some(se.size),
                            mtime_ms: Some(se.mtime_ms),
                            hash: Some(hs.clone()),
                            link: None,
                            mode: None,
                            reason: reason.into(),
                        }),
                        // sync: both sides differ in content with no attribution → report the conflict honestly, a human rules
                        "sync" => Some(Op {
                            side: Side::Target,
                            action: Action::Conflict,
                            path: se.path.clone(),
                            from: None,
                            size: Some(se.size),
                            mtime_ms: Some(se.mtime_ms),
                            hash: None,
                            link: None,
                            mode: None,
                            reason: reason.into(),
                        }),
                        // enrich: update only when source is strictly newer
                        _ => {
                            if se.mtime_ms > te.mtime_ms + slack_ms {
                                Some(Op {
                                    side: Side::Target,
                                    action: Action::Update,
                                    path: se.path.clone(),
                                    from: None,
                                    size: Some(se.size),
                                    mtime_ms: Some(se.mtime_ms),
                                    hash: Some(hs.clone()),
                                    link: None,
                                    mode: None,
                                    reason: reason.into(),
                                })
                            } else {
                                None
                            }
                        }
                    }
                };
                pp.item_done(&se.path);
                Ok((*source_index, *target_index, hs, ht, op))
            },
        )
        .collect::<std::io::Result<Vec<_>>>()?;

    // The returned snapshots are the retained comparison evidence. Once escalation has measured a
    // stronger full hash, keep it there so every later evidence view reaches the same verdict as
    // the plan instead of re-reading the obsolete sampled digest.
    for (source_index, target_index, source_hash, target_hash, _) in &escalated {
        s.entries[*source_index].hash = Some(source_hash.clone());
        t.entries[*target_index].hash = Some(target_hash.clone());
    }
    let extra: Vec<Op> = escalated
        .into_iter()
        .filter_map(|(_, _, _, _, operation)| operation)
        .collect();
    if !extra.is_empty() {
        ctx.log(LogLevel::Warn, "compare", format!("escalation confirmed {} file(s) really do differ in content (changes outside the sampling window); added to the plan", extra.len()));
        let new_conflicts = extra
            .iter()
            .filter(|o| o.action == Action::Conflict)
            .count() as u64;
        plan.header.op_count += extra.len() as u64;
        plan.header.conflict_count += new_conflicts;
        plan.ops.extend(extra);
    }
    Ok(plan)
}

/// Run the gates without executing — the GUI calls this before raising the confirmation sheet, so the refusal
/// reasons are shown to the person in full instead of landing only on a stderr nobody reads.
pub fn preflight_job(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    acknowledged: bool,
) -> std::io::Result<crate::pipeline::guard::Verdict> {
    job.validate()
        .map_err(|reason| std::io::Error::new(std::io::ErrorKind::InvalidInput, reason))?;
    let source = resolve_root(&job.source)?;
    let target = resolve_root(&job.target)?;
    Ok(preflight_resolved(
        job,
        plan,
        ops,
        acknowledged,
        &source,
        &target,
    ))
}

pub fn apply_requirements(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    acknowledged: bool,
) -> std::io::Result<super::ApplyRequirements> {
    job.validate()
        .map_err(|reason| std::io::Error::new(std::io::ErrorKind::InvalidInput, reason))?;
    let source = resolve_root(&job.source)?;
    let target = resolve_root(&job.target)?;
    Ok(apply_requirements_resolved(
        job,
        plan,
        ops,
        acknowledged,
        &source,
        &target,
    ))
}

pub fn apply_requirements_resolved(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    acknowledged: bool,
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
) -> super::ApplyRequirements {
    let verdict = preflight_resolved(job, plan, ops, acknowledged, source, target);
    let query = job.write_caps_query(source.as_local().is_some(), target.as_local().is_some());
    let capabilities =
        crate::pipeline::guard::caps::cap_report_write(&query, ops, &source.caps(), &target.caps());
    super::ApplyRequirements {
        verdict,
        capabilities,
    }
}

pub fn preflight_resolved(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    acknowledged: bool,
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
) -> crate::pipeline::guard::Verdict {
    let mut verdict = crate::pipeline::guard::Verdict {
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    let source_label = root_label(source);
    let target_label = root_label(target);
    if source_label != plan.header.source_root || target_label != plan.header.target_root {
        verdict.blockers.push(format!(
            "this plan was made for '{}' → '{}' but the job resolves to '{}' → '{}' — run Compare again",
            plan.header.source_root,
            plan.header.target_root,
            source_label,
            target_label,
        ));
        return verdict;
    }
    crate::pipeline::guard::run_all_vfs(
        ops,
        source,
        target,
        &plan.header,
        &job.guards(acknowledged),
    )
}

fn root_label(vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>) -> String {
    vfs.as_local()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| vfs.display())
}

/// The refusal shape both halves of the apply gate share: one `Error` event, and an outcome that did
/// nothing. A module-level fn rather than a closure because the phrase half (a root that will not
/// open) and the backend half (a capability report that blocks) both have to raise it.
fn refuse_apply(
    ctx: &crate::obs::progress::RunCtx,
    ops_len: usize,
    action: &str,
    message: String,
) -> crate::obs::progress::ApplyOutcome {
    use crate::model::event::{Phase, ProgressEvent};
    ctx.sink.emit(ProgressEvent::Error {
        phase: Phase::Apply,
        ts_ms: crate::foundation::time::now_ms(),
        path: String::new(),
        action: action.into(),
        side: "target".into(),
        message,
    });
    crate::obs::progress::ApplyOutcome {
        done: 0,
        skipped: ops_len as u64,
        errors: 1,
        bytes_copied: 0,
        cancelled: false,
    }
}

/// Every apply invocation has one terminal event, including failures before `Phase::Apply` can
/// start. The progress window derives its running state from this boundary; without it a refused
/// run keeps spinning after the engine has already released the run slot.
fn finish_apply(
    ctx: &crate::obs::progress::RunCtx,
    t0: std::time::Instant,
    mut out: crate::obs::progress::ApplyOutcome,
) -> crate::obs::progress::ApplyOutcome {
    out.cancelled |= ctx.ctl.cancelled();
    ctx.sink.emit(crate::model::event::ProgressEvent::Summary {
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
    apply_job_guarded_with_consent(
        job,
        plan,
        ops,
        trash,
        verbose,
        acknowledged,
        &crate::pipeline::guard::caps::CapabilityConsent::explicit_cli(accept_caps),
        ctx,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn apply_job_guarded_with_consent(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    apply_job_guarded_with_consent_classified(
        job,
        plan,
        ops,
        trash,
        verbose,
        acknowledged,
        consent,
        ctx,
    )
    .into_result()
    .expect("the local apply lane represents every refusal as an ApplyOutcome")
}

#[allow(clippy::too_many_arguments)]
pub fn apply_job_guarded_with_consent_classified(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
    ctx: &crate::obs::progress::RunCtx,
) -> super::ApplyExecution {
    let t0 = std::time::Instant::now();
    let sv = match resolve_root(&job.source) {
        Ok(v) => v,
        Err(e) => {
            return super::ApplyExecution::rejected(finish_apply(
                ctx,
                t0,
                refuse_apply(ctx, ops.len(), "resolve-roots", e.to_string()),
            ))
        }
    };
    let tv = match resolve_root(&job.target) {
        Ok(v) => v,
        Err(e) => {
            return super::ApplyExecution::rejected(finish_apply(
                ctx,
                t0,
                refuse_apply(ctx, ops.len(), "resolve-roots", e.to_string()),
            ))
        }
    };
    apply_resolved_with_consent_classified(
        job,
        plan,
        ops,
        &sv,
        &tv,
        trash,
        verbose,
        acknowledged,
        consent,
        t0,
        ctx,
    )
}

/// Apply a plan to two roots that are already open. Split out from `apply_job_guarded_with` the same
/// way, and for the same reason, `compare_resolved` was split from `compare_job_detailed`:
/// everything below here works on backends, not spellings — which is what lets the write lane be
/// exercised against an in-memory root instead of only against a phrase naming a real disk.
///
/// `t0` belongs to the caller, so the Summary still measures from before the roots were opened.
#[allow(clippy::too_many_arguments)] // every one is a distinct decision the caller has already made
pub fn apply_resolved(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
    t0: std::time::Instant,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    apply_resolved_with_consent(
        job,
        plan,
        ops,
        sv,
        tv,
        trash,
        verbose,
        acknowledged,
        &crate::pipeline::guard::caps::CapabilityConsent::explicit_cli(accept_caps),
        t0,
        ctx,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn apply_resolved_with_consent(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
    t0: std::time::Instant,
    ctx: &crate::obs::progress::RunCtx,
) -> crate::obs::progress::ApplyOutcome {
    apply_resolved_with_consent_classified(
        job,
        plan,
        ops,
        sv,
        tv,
        trash,
        verbose,
        acknowledged,
        consent,
        t0,
        ctx,
    )
    .into_result()
    .expect("the local apply lane represents every refusal as an ApplyOutcome")
}

#[allow(clippy::too_many_arguments)]
pub fn apply_resolved_with_consent_classified(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
    t0: std::time::Instant,
    ctx: &crate::obs::progress::RunCtx,
) -> super::ApplyExecution {
    use crate::model::event::{Phase, ProgressEvent};
    use crate::obs::progress::ApplyOutcome;
    // The plan must be the one made for THESE roots. The header carries the label the
    // scan wrote: the local (possibly translated) path for local lanes, the display
    // phrase for generic-lane roots.
    if root_label(sv) != plan.header.source_root || root_label(tv) != plan.header.target_root {
        let out = refuse_apply(
            ctx,
            ops.len(),
            "resolve-roots",
            format!(
                "this plan was made for '{}' → '{}' but the job resolves to '{}' → '{}' — run compare again",
                plan.header.source_root,
                plan.header.target_root,
                root_label(sv),
                root_label(tv)
            ),
        );
        return super::ApplyExecution::rejected(finish_apply(ctx, t0, out));
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
            let out = refuse_apply(
                ctx,
                ops.len(),
                "caps",
                blockers
                    .iter()
                    .map(|i| i.render())
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            return super::ApplyExecution::rejected(finish_apply(ctx, t0, out));
        }
        let acks = wr.needs_ack();
        if !wr.consent_satisfied(
            crate::pipeline::guard::caps::CapabilityScope::ApplyWrite,
            consent,
        ) {
            let instruction = match consent {
                crate::pipeline::guard::caps::CapabilityConsent::ExactDigest(_) => {
                    "the capability report changed after it was authorized — review Apply again"
                }
                _ => "rerun with --accept-caps to consent",
            };
            let out =
                refuse_apply(
                    ctx,
                    ops.len(),
                    "caps",
                    format!(
                    "this apply degrades on capabilities the backends lack — {instruction}:\n  {}",
                    acks.iter().map(|i| i.render()).collect::<Vec<_>>().join("\n  ")
                ),
                );
            return super::ApplyExecution::rejected(finish_apply(ctx, t0, out));
        }
    }
    let verdict =
        crate::pipeline::guard::run_all_vfs(ops, sv, tv, &plan.header, &job.guards(acknowledged));
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
        let out = ApplyOutcome {
            done: 0,
            skipped: ops.len() as u64,
            errors: 1,
            bytes_copied: 0,
            cancelled: false,
        };
        return super::ApplyExecution::rejected(finish_apply(ctx, t0, out));
    }
    let ap = apply::apply_vfs(ops, sv, tv, &job.apply_opts(trash, verbose), ctx);
    let mut out = ApplyOutcome {
        cancelled: ctx.ctl.cancelled(),
        ..ap
    };
    // A cancelled run does not refresh the archive: the user asked to "stop now", and re-reporting conflicts next round is safe anyway
    if out.errors == 0 && !out.cancelled && job.mode == "sync" {
        let refreshed =
            refresh_archive_with(job, plan, sv, &super::effective_scan_opts(job, sv, tv), ctx);
        out.cancelled = ctx.ctl.cancelled();
        if !refreshed {
            if !out.cancelled {
                out.errors += 1;
            }
        }
    }
    super::ApplyExecution::started(finish_apply(ctx, t0, out))
}

/// End-to-end run for local/mounted-disk jobs (the body of the original CLI run). Returns (done, skipped, errors, conflicts).
pub fn run_local_job(
    name: &str,
    job: &Job,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
) -> std::io::Result<(u64, u64, u64, u64)> {
    // 1:N (the original requirement): one source → each target compared and executed independently.
    // One plan and one run log per target; source-side hashing is absorbed by the cache (in the fast tier, near-zero reads from the second target on).
    job.validate()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let targets = job.target_list();
    let multi = targets.len() > 1;
    let mut tot = (0u64, 0u64, 0u64, 0u64);
    for (i, t) in targets.iter().enumerate() {
        let jt = job.for_target(t);
        let label = if multi {
            format!("{name}[{}/{} → {t}]", i + 1, targets.len())
        } else {
            name.to_string()
        };
        let r = run_local_single(&label, &jt, do_apply, verbose, acknowledged, accept_caps)?;
        tot.0 += r.0;
        tot.1 += r.1;
        tot.2 += r.2;
        tot.3 += r.3;
    }
    Ok(tot)
}

pub fn run_local_single(
    name: &str,
    job: &Job,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
) -> std::io::Result<(u64, u64, u64, u64)> {
    let plan = compare_job_detailed(job, &crate::obs::progress::RunCtx::null(), accept_caps)?.plan;
    crate::log_info!(
        "run",
        "[{name}] {} op(s), {} conflict(s)",
        plan.header.op_count,
        plan.header.conflict_count
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
    // M4: the CLI's apply leaves a run log too (desktop records its own at the shell layer)
    let t0 = std::time::Instant::now();
    let rec = crate::obs::runlog::Recorder::start(
        name,
        "apply",
        &crate::obs::progress::RunCtx::null(),
        &ops,
    );
    let out = apply_job_guarded_with(
        job,
        &plan,
        &ops,
        None,
        verbose,
        acknowledged,
        accept_caps,
        &rec.ctx,
    );
    rec.finish(&out, t0.elapsed().as_millis() as u64);
    Ok((
        out.done,
        out.skipped,
        out.errors,
        plan.header.conflict_count,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::memory::MemVfs;
    use crate::fs::vfs::Vfs;
    use crate::model::event::{Phase, PhaseStatus, ProgressEvent};
    use crate::obs::progress::{PhaseProgress, RunCtl, RunCtx};
    use std::sync::{Arc, Mutex};

    fn escalation_fixture(
        tag: &str,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        Snapshot,
        Snapshot,
        Plan,
        Job,
    ) {
        let base =
            std::env::temp_dir().join(format!("syncdash-escalation-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let source = base.join("source");
        let target = base.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(source.join("suspect.bin"), b"same").unwrap();
        std::fs::write(target.join("suspect.bin"), b"same").unwrap();

        let scan_one = |root: &std::path::Path| {
            scan::scan(
                root,
                &scan::ScanOptions {
                    hash: false,
                    sampled: false,
                    use_cache: false,
                    symlinks_direct: false,
                    filter: crate::pipeline::filter::PathFilter::build(&[], &[]),
                },
            )
            .unwrap()
        };
        let mut source_snapshot = scan_one(&source);
        let mut target_snapshot = scan_one(&target);
        let source_entry = source_snapshot
            .entries
            .iter_mut()
            .find(|entry| entry.path == "suspect.bin")
            .unwrap();
        source_entry.hash = Some("~same-sample".into());
        source_entry.mtime_ms = 10_000;
        let target_entry = target_snapshot
            .entries
            .iter_mut()
            .find(|entry| entry.path == "suspect.bin")
            .unwrap();
        target_entry.hash = Some("~same-sample".into());
        target_entry.mtime_ms = 0;
        source_snapshot.header.hashed = true;
        target_snapshot.header.hashed = true;

        let mut job = Job::default();
        job.mode = "mirror".into();
        job.rigor = "fast".into();
        let plan = compare::compare(
            &source_snapshot,
            &target_snapshot,
            &job.mode,
            None,
            false,
            &job.compare_opts(),
        );
        assert!(
            plan.ops.is_empty(),
            "the sampled evidence alone calls this pair identical"
        );
        (source, target, source_snapshot, target_snapshot, plan, job)
    }

    #[test]
    fn same_volume_scan_stops_before_target_when_source_fails() {
        let target_called = std::sync::atomic::AtomicBool::new(false);
        let result: std::io::Result<((), ())> = schedule_scans(
            true,
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "source failed",
                ))
            },
            || {
                target_called.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        );

        assert_eq!(result.unwrap_err().to_string(), "source failed");
        assert!(!target_called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn escalation_read_failure_aborts_instead_of_retaining_identical() {
        let (source, target, mut source_snapshot, mut target_snapshot, plan, job) =
            escalation_fixture("read-failure");
        std::fs::remove_file(source.join("suspect.bin")).unwrap();
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctx = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |event| copy.lock().unwrap().push(event)),
        );
        let pp = PhaseProgress::begin(&ctx, Phase::Compare, None, 0, 0);
        let source_vfs =
            Arc::new(crate::fs::vfs::local::LocalVfs::new(source.clone())) as Arc<dyn Vfs>;
        let target_vfs =
            Arc::new(crate::fs::vfs::local::LocalVfs::new(target.clone())) as Arc<dyn Vfs>;

        let error = match escalate_sampled_disagreements(
            &job,
            plan,
            &mut source_snapshot,
            &mut target_snapshot,
            &ctx,
            &source_vfs,
            &target_vfs,
            &pp,
        ) {
            Ok(_) => panic!("an unreadable full-verification file must abort comparison"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(
            error
                .to_string()
                .contains("cannot fully verify source 'suspect.bin'"),
            "{error}"
        );
        drop(pp);

        let events = events.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            ProgressEvent::Totals {
                phase: Phase::Compare,
                items_total: 1,
                bytes_total: 8,
                ..
            }
        )));
        assert!(matches!(
            events.last(),
            Some(ProgressEvent::PhaseEnd {
                phase: Phase::Compare,
                status: PhaseStatus::Failed,
                ..
            })
        ));
        let _ = std::fs::remove_dir_all(source.parent().unwrap());
    }

    #[test]
    fn escalation_honors_cancellation_before_reopening_files() {
        let (source, target, mut source_snapshot, mut target_snapshot, plan, job) =
            escalation_fixture("cancel");
        let ctl = RunCtl::new();
        ctl.request_cancel();
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctx = RunCtx::new(ctl, Arc::new(move |event| copy.lock().unwrap().push(event)));
        let pp = PhaseProgress::begin(&ctx, Phase::Compare, None, 0, 0);
        let source_vfs =
            Arc::new(crate::fs::vfs::local::LocalVfs::new(source.clone())) as Arc<dyn Vfs>;
        let target_vfs =
            Arc::new(crate::fs::vfs::local::LocalVfs::new(target.clone())) as Arc<dyn Vfs>;

        let error = match escalate_sampled_disagreements(
            &job,
            plan,
            &mut source_snapshot,
            &mut target_snapshot,
            &ctx,
            &source_vfs,
            &target_vfs,
            &pp,
        ) {
            Ok(_) => panic!("cancelled escalation must not complete"),
            Err(error) => error,
        };
        assert!(crate::obs::progress::is_cancelled(&error));
        drop(pp);
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(ProgressEvent::PhaseEnd {
                phase: Phase::Compare,
                status: PhaseStatus::Cancelled,
                ..
            })
        ));
        let _ = std::fs::remove_dir_all(source.parent().unwrap());
    }

    #[test]
    fn escalation_rechecks_nonlocal_vfs_roots_instead_of_skipping_them() {
        let source =
            MemVfs::new("escalate-vfs-source").without(|caps| caps.max_parallel_streams = 1);
        let target =
            MemVfs::new("escalate-vfs-target").without(|caps| caps.max_parallel_streams = 1);
        source.seed_bytes("suspect.bin", b"src!", 10_000);
        target.seed_bytes("suspect.bin", b"tgt!", 0);
        let source = Arc::new(source) as Arc<dyn Vfs>;
        let target = Arc::new(target) as Arc<dyn Vfs>;
        let opt = scan::ScanOptions {
            hash: false,
            sampled: false,
            use_cache: false,
            symlinks_direct: false,
            filter: crate::pipeline::filter::PathFilter::build(&[], &[]),
        };
        let mut source_snapshot =
            scan::scan_root(&source, &opt, &RunCtx::null(), Phase::ScanSource).unwrap();
        let mut target_snapshot =
            scan::scan_root(&target, &opt, &RunCtx::null(), Phase::ScanTarget).unwrap();
        source_snapshot.entries[0].hash = Some("~same-sample".into());
        target_snapshot.entries[0].hash = Some("~same-sample".into());
        source_snapshot.header.hashed = true;
        target_snapshot.header.hashed = true;
        let mut job = Job::default();
        job.mode = "mirror".into();
        job.rigor = "fast".into();
        let plan = compare::compare(
            &source_snapshot,
            &target_snapshot,
            &job.mode,
            None,
            false,
            &job.compare_opts(),
        );
        assert!(plan.ops.is_empty());
        let ctx = RunCtx::null();
        let pp = PhaseProgress::begin(&ctx, Phase::Compare, None, 0, 0);

        let plan = escalate_sampled_disagreements(
            &job,
            plan,
            &mut source_snapshot,
            &mut target_snapshot,
            &ctx,
            &source,
            &target,
            &pp,
        )
        .unwrap();
        pp.finish().unwrap();

        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].action, Action::Update);
        assert!(plan.ops[0].reason.starts_with("escalated:"));
        let evidence = compare::evidence::evidence(
            &source_snapshot,
            &target_snapshot,
            &plan,
            &job.compare_opts(),
        );
        assert_eq!(evidence.identical_count, 0);
    }

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

        assert_eq!(
            out.source.header.excluded_dirs, 1,
            "a pruned subtree must be counted, never silent"
        );
        assert_eq!(out.target.header.excluded_dirs, 1);
        assert!(
            out.source.header.vfs.is_some(),
            "a VFS root's snapshot must carry its self-description"
        );
        assert!(
            !out.source
                .entries
                .iter()
                .any(|e| e.path.starts_with("skipme")),
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

    #[test]
    fn preflight_uses_the_open_vfs_roots_instead_of_display_paths() {
        let source = MemVfs::new("preflight-source");
        let target = MemVfs::new("preflight-target");
        source.seed_bytes(crate::foundation::names::MARKER_NAME, b"source", 1);
        target.seed_bytes(crate::foundation::names::MARKER_NAME, b"target", 1);
        let (source, target) = (
            Arc::new(source) as Arc<dyn Vfs>,
            Arc::new(target) as Arc<dyn Vfs>,
        );
        let mut job = Job::default();
        job.require_marker = true;
        let plan = compare_resolved(&job, &source, &target, &RunCtx::null(), false)
            .unwrap()
            .plan;

        let verdict = preflight_resolved(&job, &plan, &[], false, &source, &target);

        assert!(
            verdict.ok(),
            "VFS markers must satisfy preflight: {:?}",
            verdict.blockers
        );
    }

    #[test]
    fn preflight_rejects_a_plan_for_different_resolved_roots() {
        let source = Arc::new(MemVfs::new("preflight-source")) as Arc<dyn Vfs>;
        let target = Arc::new(MemVfs::new("preflight-target")) as Arc<dyn Vfs>;
        let job = Job::default();
        let mut plan = compare_resolved(&job, &source, &target, &RunCtx::null(), false)
            .unwrap()
            .plan;
        plan.header.target_root = "mem://another-target".into();

        let verdict = preflight_resolved(&job, &plan, &[], false, &source, &target);

        assert!(!verdict.ok());
        assert!(verdict.blockers[0].contains("run Compare again"));

        let execution = apply_resolved_with_consent_classified(
            &job,
            &plan,
            &[],
            &source,
            &target,
            None,
            false,
            false,
            &crate::pipeline::guard::caps::CapabilityConsent::None,
            std::time::Instant::now(),
            &RunCtx::null(),
        );
        assert!(
            !execution.writes_started(),
            "a changed root label is rejected before the write lane"
        );
        assert_eq!(execution.into_result().unwrap().errors, 1);
    }

    #[test]
    fn capability_and_health_refusals_are_classified_before_write() {
        let source = Arc::new(MemVfs::new("gate-source")) as Arc<dyn Vfs>;
        let target = Arc::new(MemVfs::new("gate-target").without(|caps| {
            caps.symlink = crate::fs::vfs::Support::No;
        })) as Arc<dyn Vfs>;
        let job = Job::default();
        let mut plan = compare_resolved(&job, &source, &target, &RunCtx::null(), false)
            .unwrap()
            .plan;
        let symlink = Op {
            side: crate::model::plan::Side::Target,
            action: Action::Copy,
            path: "link".into(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: Some("destination".into()),
            mode: None,
            reason: "test capability boundary".into(),
        };
        plan.ops.push(symlink.clone());
        let capability_refusal = apply_resolved_with_consent_classified(
            &job,
            &plan,
            &[symlink],
            &source,
            &target,
            None,
            false,
            false,
            &crate::pipeline::guard::caps::CapabilityConsent::ExplicitCli,
            std::time::Instant::now(),
            &RunCtx::null(),
        );
        assert!(!capability_refusal.writes_started());
        assert_eq!(capability_refusal.into_result().unwrap().errors, 1);

        let source = Arc::new(MemVfs::new("health-source")) as Arc<dyn Vfs>;
        let target = Arc::new(MemVfs::new("health-target")) as Arc<dyn Vfs>;
        let healthy_job = Job::default();
        let plan = compare_resolved(&healthy_job, &source, &target, &RunCtx::null(), false)
            .unwrap()
            .plan;
        let mut marker_required = healthy_job;
        marker_required.require_marker = true;
        let health_refusal = apply_resolved_with_consent_classified(
            &marker_required,
            &plan,
            &[],
            &source,
            &target,
            None,
            false,
            false,
            &crate::pipeline::guard::caps::CapabilityConsent::ExplicitCli,
            std::time::Instant::now(),
            &RunCtx::null(),
        );
        assert!(!health_refusal.writes_started());
        assert_eq!(health_refusal.into_result().unwrap().errors, 1);
    }

    #[test]
    fn entering_the_local_write_lane_is_never_a_safe_rejection() {
        let source_mem = MemVfs::new("write-source");
        source_mem.seed_bytes("new.txt", b"content", 1_000);
        let target_mem = MemVfs::new("write-target");
        let source = Arc::new(source_mem) as Arc<dyn Vfs>;
        let target = Arc::new(target_mem) as Arc<dyn Vfs>;
        let job = Job::default();
        let plan = compare_resolved(&job, &source, &target, &RunCtx::null(), false)
            .unwrap()
            .plan;
        assert_eq!(plan.ops.len(), 1);

        let execution = apply_resolved_with_consent_classified(
            &job,
            &plan,
            &plan.ops,
            &source,
            &target,
            None,
            false,
            false,
            &crate::pipeline::guard::caps::CapabilityConsent::ExplicitCli,
            std::time::Instant::now(),
            &RunCtx::null(),
        );
        assert!(execution.writes_started());
        let outcome = execution.into_result().unwrap();
        assert_eq!(outcome.errors, 0);
        assert_eq!(outcome.done, 1);
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
        assert_eq!(
            out.source.header.vfs.as_ref().unwrap().evidence_effective,
            "full"
        );
        assert_eq!(
            out.target.header.vfs.as_ref().unwrap().evidence_effective,
            "full"
        );
        assert!(
            !out.target.header.vfs.as_ref().unwrap().degraded.is_empty(),
            "the consented degradation must ride on the snapshot"
        );
        assert_eq!(
            out.plan.ops.len(),
            0,
            "identical content must not produce ops after the joint upgrade"
        );
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

    #[test]
    fn apply_resolution_refusal_emits_exactly_one_terminal_summary() {
        let sv = Arc::new(MemVfs::new("terminal-src")) as Arc<dyn Vfs>;
        let tv = Arc::new(MemVfs::new("terminal-tgt")) as Arc<dyn Vfs>;
        let job = Job::default();
        let plan = compare_resolved(&job, &sv, &tv, &RunCtx::null(), false)
            .unwrap()
            .plan;

        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctx = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |ev| copy.lock().unwrap().push(ev)),
        );
        let mut broken = job;
        broken.source = "sfpt://typo/data".into();
        let execution = apply_job_guarded_with_consent_classified(
            &broken,
            &plan,
            &[],
            None,
            false,
            false,
            &crate::pipeline::guard::caps::CapabilityConsent::None,
            &ctx,
        );
        assert!(
            !execution.writes_started(),
            "a root that cannot open is a proven pre-write refusal"
        );
        let out = execution.into_result().unwrap();

        assert_eq!(out.errors, 1);
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ProgressEvent::Summary { .. }))
                .count(),
            1
        );
        assert!(matches!(
            events.last(),
            Some(ProgressEvent::Summary {
                errors: 1,
                cancelled: false,
                ..
            })
        ));
    }

    #[test]
    fn terminal_summary_observes_a_last_moment_cancel_request() {
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctl = RunCtl::new();
        ctl.request_cancel();
        let ctx = RunCtx::new(ctl, Arc::new(move |ev| copy.lock().unwrap().push(ev)));

        let out = finish_apply(
            &ctx,
            std::time::Instant::now(),
            crate::obs::progress::ApplyOutcome::default(),
        );
        assert!(out.cancelled);
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(ProgressEvent::Summary {
                cancelled: true,
                ..
            })
        ));
    }
}
