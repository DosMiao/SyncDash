//! Read-only Compare orchestration for roots opened by this process.
//!
//! "Local" is about the transport, not the roots: an `sftp://` target still runs here, because
//! this process does the reading and writing. What makes a job *not* local is a peer that runs
//! syncdash itself — see `peer`.

use crate::job::{Job, SingleTargetJob};
use crate::model::plan::{Action, Op, Plan};
use crate::model::table::TableArtifact;
use crate::pipeline::{compare, scan};

use super::super::roots::resolve_root;
use super::super::CompareOutcome;

pub(super) fn schedule_scans<S, T, FS, FT>(
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

/// The same pipeline, but it also hands back both snapshots (throwing them away would force the UI
/// to scan all over again).
pub fn compare_job_detailed(
    job: &SingleTargetJob,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<CompareOutcome> {
    let configuration = job.configuration();
    // Resolve both roots to live backends before anything else: local stays local, a
    // translating backend mounts, and a network/VFS endpoint connects — Auth/unreachable
    // errors surface here, never mid-scan.
    let sv = resolve_root(&configuration.source)?;
    let tv = resolve_root(job.target())?;
    compare_resolved(configuration, &sv, &tv, ctx)
}

pub fn compare_capabilities(
    job: &SingleTargetJob,
) -> std::io::Result<crate::pipeline::guard::caps::CapReport> {
    let configuration = job.configuration();
    let source = resolve_root(&configuration.source)?;
    let target = resolve_root(job.target())?;
    let window_ms = configuration
        .compare_opts()
        .mtime_window_ms
        .max(source.caps().mtime_precision_ms as i64)
        .max(target.caps().mtime_precision_ms as i64);
    Ok(read_capabilities_resolved(
        configuration,
        &source,
        &target,
        window_ms,
    ))
}

fn read_capabilities_resolved(
    job: &Job,
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    window_ms: i64,
) -> crate::pipeline::guard::caps::CapReport {
    let query = job.read_caps_query(
        window_ms,
        source.local_root().is_some(),
        target.local_root().is_some(),
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
) -> std::io::Result<CompareOutcome> {
    use crate::model::event::Phase;
    let opt = super::super::effective_scan_opts(job, sv, tv);
    // When a share is not mounted, its path often looks like an empty directory,
    // and comparing as usual yields a plan that either "wipes the other side" or "re-sends everything".
    let mut v = crate::pipeline::guard::Verdict::default();
    crate::pipeline::guard::roots::check_root_vfs("source", sv, job.require_marker, &mut v);
    crate::pipeline::guard::roots::check_root_vfs("target", tv, job.require_marker, &mut v);
    super::super::fail_on_root_blockers(&v, ctx, None)?;
    // The no-hash equality window widens to the coarser of the two backends' declared
    // mtime precision (an FTP LIST root thinks in minutes). Hash evidence is unaffected.
    // This widened value is the window: it is what `compare` judges on, what escalation and the
    // capability list read, and — through the returned `compare_options` — the number the desktop
    // publishes as `PlanDto.mtime_window_ms`. Nothing downstream may re-derive it from the policy
    // default.
    let mut copts = job.compare_opts();
    copts.mtime_window_ms = copts
        .mtime_window_ms
        .max(sv.caps().mtime_precision_ms as i64)
        .max(tv.caps().mtime_precision_ms as i64);
    // The capability report: every gap between what the job asks and what the backends can give
    // is listed BEFORE any scanning, so nothing degrades silently. It is a list, not a gate —
    // the comparison is read-only and proceeds on its own.
    let caps_report = read_capabilities_resolved(job, sv, tv, copts.mtime_window_ms);
    super::log_capability_list(ctx, "compare", &caps_report);
    // Different volumes/links scan in parallel, so wall clock is approximately the slower side.
    // Two roots on the same mounted volume scan sequentially to avoid competing metadata walks;
    // this is a filesystem identity (`st_dev`/volume root), not a claim about the physical disk.
    let same_local_volume = match (sv.local_root(), tv.local_root()) {
        (Some(source), Some(target)) => {
            crate::foundation::volume::same_device(source.display_path(), target.display_path())
        }
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
    let archive = super::super::load_comparison_archive(job, &opt, ctx)?;
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
    // Disagreement escalation: in the sampled evidence tier, a file whose digests match but whose
    // mtimes fall outside the effective window above may not simply be ruled identical (the knob
    // can turn this off)
    let rr = job.rigor_resolved();
    let plan = if rr.sampled && rr.escalate {
        escalate_sampled_disagreements(
            job,
            plan,
            EscalationSide {
                table: &mut s,
                vfs: sv,
            },
            EscalationSide {
                table: &mut t,
                vfs: tv,
            },
            ctx,
            &pp,
        )?
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

/// One side of an escalation: the snapshot whose evidence is corrected, together with the backend
/// the file is re-read through.
///
/// They travel as one value because they must describe the same root. As two independent
/// parameters, handing the source table with the target backend type-checks, and the resulting
/// plan would be built from a full hash of the wrong file.
pub(super) struct EscalationSide<'a> {
    pub(super) table: &'a mut TableArtifact,
    pub(super) vfs: &'a std::sync::Arc<dyn crate::fs::vfs::Vfs>,
}

/// The escalation rule: when the two signals fight (the sampled digest says "identical", mtime says "touched"), believe neither silently —
/// **escalate that file to a full hash on both sides** and rule again. This shrinks the blind spot of fast/balanced/standard from
/// "any change outside the sampling window" to "outside the sampling window *and* timestamp-preserving" (≈ the timestomp case).
/// The escalation set is naturally tiny (near zero on a normal tree), so reading each one in full
/// costs next to nothing. Reads stay on the VFS surface so local and protocol roots get the same
/// safety rule and the same classified failures.
pub(super) fn escalate_sampled_disagreements(
    job: &Job,
    mut plan: Plan,
    source: EscalationSide<'_>,
    target: EscalationSide<'_>,
    ctx: &crate::obs::progress::RunCtx,
    pp: &crate::obs::progress::PhaseProgress<'_>,
) -> std::io::Result<Plan> {
    use crate::model::digest::Blake3Digest;
    use crate::model::event::LogLevel;
    use crate::model::plan::Side;
    use crate::model::table::{FileIdentityObservation, ObservedFile};

    let EscalationSide {
        table: s,
        vfs: source,
    } = source;
    let EscalationSide {
        table: t,
        vfs: target,
    } = target;
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
    ) -> std::io::Result<Blake3Digest> {
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
        Ok(Blake3Digest::from_hash(h.finalize()))
    }
    let target_by_path: std::collections::HashMap<&str, (usize, &ObservedFile)> = t
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            entry
                .as_file()
                .map(|file| (file.path.as_str(), (index, file)))
        })
        .collect();
    let suspects: Vec<(usize, usize, ObservedFile, ObservedFile)> = s
        .entries
        .iter()
        .enumerate()
        .filter_map(|(source_index, entry)| {
            let source_entry = entry.as_file()?;
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
            matches!(
                (&source_entry.identity, &target_entry.identity),
                (
                    FileIdentityObservation::SampledBlake3 { digest: left },
                    FileIdentityObservation::SampledBlake3 { digest: right }
                ) if left == right
                    && (source_entry.mtime_ms - target_entry.mtime_ms).abs() > slack_ms
            )
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
    let escalated: Vec<(usize, usize, Blake3Digest, Blake3Digest, Option<Op>)> = suspects
        .iter()
        .map(
            |(source_index, target_index, se, te)| -> std::io::Result<(
                usize,
                usize,
                Blake3Digest,
                Blake3Digest,
                Option<Op>,
            )> {
                let hs = full_hash(source, Side::Source.as_str(), se.path.as_str(), pp)?;
                let ht = full_hash(target, Side::Target.as_str(), te.path.as_str(), pp)?;
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
                            path: se.path.as_str().to_owned(),
                            from: None,
                            size: Some(se.size),
                            mtime_ms: Some(se.mtime_ms),
                            hash: Some(hs.as_str().to_owned()),
                            link: None,
                            mode: None,
                            reason: reason.into(),
                        }),
                        // sync: both sides differ in content with no attribution → report the conflict honestly, a human rules
                        "sync" => Some(Op {
                            side: Side::Target,
                            action: Action::Conflict,
                            path: se.path.as_str().to_owned(),
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
                                    path: se.path.as_str().to_owned(),
                                    from: None,
                                    size: Some(se.size),
                                    mtime_ms: Some(se.mtime_ms),
                                    hash: Some(hs.as_str().to_owned()),
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
                pp.item_done(se.path.as_str());
                Ok((*source_index, *target_index, hs, ht, op))
            },
        )
        .collect::<std::io::Result<Vec<_>>>()?;

    // The returned snapshots are the retained comparison evidence. Once escalation has measured a
    // stronger full hash, keep it there so every later evidence view reaches the same verdict as
    // the plan instead of re-reading the obsolete sampled digest.
    for (source_index, target_index, source_hash, target_hash, _) in &escalated {
        s.entries[*source_index].as_file_mut().unwrap().identity =
            FileIdentityObservation::FullBlake3 {
                digest: source_hash.clone(),
            };
        t.entries[*target_index].as_file_mut().unwrap().identity =
            FileIdentityObservation::FullBlake3 {
                digest: target_hash.clone(),
            };
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
