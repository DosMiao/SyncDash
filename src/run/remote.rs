//! Jobs that execute on a peer over ssh.
//!
//! The far side runs syncdash against its own disk: it scans there and sends back a table, and it
//! applies a package we build here. That is why the capability consent and the disk-space gate do
//! not apply to this lane — they are questions about roots this process opened, and it opened none.

use crate::job::Job;
use crate::model::plan::{Action, Op, Plan};
use crate::pipeline::{compare, scan};

use super::{scan_opts, CompareOutcome};
use super::archive::refresh_archive_with;
use crate::model::table::Snapshot;

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
    let plan = match super::compare_remote_job_with(name, job, ctx) {
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
    /// A local path serving the *same* tree the peer syncs — the `|mount=` option.
    ///
    /// The peer lane pushes: it packs the target-side ops and the far side applies them. The
    /// reverse (source-side) direction has nothing to push, so it writes through this mount
    /// instead. It is an option on the phrase rather than an assumption because a peer job used
    /// to depend on it silently: the mount lived in `target` alongside an unrelated
    /// `remote_root`, nothing said the two named one tree, and a missing mount skipped those ops
    /// with a warning nobody had a reason to expect.
    pub mount: Option<std::path::PathBuf>,
    pub shell: crate::transfer::remote::RemoteShell,
}

/// Pull the link out of a `peer://` target phrase.
fn link_of(job: &Job) -> std::io::Result<(String, String, String, Option<std::path::PathBuf>)> {
    use crate::fs::vfs::spec::{parse, RootSpec};
    let bad = |m: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, m);
    let RootSpec::Remote(r) = parse(&job.target) else {
        return Err(bad(format!("target '{}' is not a peer:// root", job.target)));
    };
    if r.root.is_empty() {
        return Err(bad(format!(
            "target '{}' names no path on {} — a peer root needs one (peer://{}/path/to/tree)",
            job.target, r.host, r.host
        )));
    }
    Ok((
        r.host.clone(),
        r.opt("exe").filter(|e| !e.is_empty()).unwrap_or("syncdash").to_string(),
        r.root.clone(),
        r.opt("mount").filter(|m| !m.is_empty()).map(std::path::PathBuf::from),
    ))
}

/// Stage 1: probe reachability + schema agreement + the remote OS (which decides the shell dialect).
///
/// It takes `ctx` for the sake of the schema-mismatch warning: that one has to reach the UI **during compare**.
/// Going through the macro and the global registry, no sink is installed during compare (only apply starts a Recorder),
/// so this line in particular would fall back to stderr — which in a windowed desktop build is the same as saying nothing.
pub fn probe_remote(name: &str, job: &Job, ctx: &crate::obs::progress::RunCtx) -> std::io::Result<RemoteLink> {
    let (host, exe, rroot, mount) = link_of(job)?;
    let (host, exe, rroot) = (host.as_str(), exe.as_str(), rroot.as_str());
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
        mount,
        shell: crate::transfer::remote::RemoteShell::from_os(&remote_os),
    })
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
    crate::pipeline::guard::roots::check_root("source", job.source_path(), job.require_marker, &mut v);
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
    let st = crate::pipeline::guard::stats::stat_plan(ops);
    let mut gv = crate::pipeline::guard::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::pipeline::guard::ratio::check_delete_ratio("target", &st.target, plan.header.target_entries, &g, &mut gv);
    crate::pipeline::guard::ratio::check_delete_ratio("source", &st.source, plan.header.source_entries, &g, &mut gv);
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

    // 5) Source side (sync's pull direction). The peer lane only pushes — it packs ops for the far
    // side to apply — so a pull has to read the remote tree through a mount of it, named by
    // `|mount=` on the phrase. No mount means the job never had a pull path; say so plainly
    // rather than reporting ops as skipped for a reason the config does not mention.
    let src_ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| o.side == Side::Source && !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    if !src_ops.is_empty() {
        match link.mount.as_deref() {
            Some(m) if m.is_dir() => {
                let out = crate::pipeline::apply::apply_with(
                    &src_ops,
                    job.source_path(),
                    m,
                    &job.apply_opts(None, verbose),
                    ctx,
                );
                done += out.done;
                skipped += out.skipped;
                errors += out.errors;
                bytes_done_total += out.bytes_copied;
            }
            Some(m) => {
                skipped += src_ops.len() as u64;
                ctx.log(
                    crate::model::event::LogLevel::Warn,
                    "remote",
                    format!(
                        "[{name}] {} pull op(s) skipped: the declared mount '{}' is not reachable — check the share, or drop |mount= if this job only pushes",
                        src_ops.len(),
                        m.display()
                    ),
                );
            }
            None => {
                skipped += src_ops.len() as u64;
                ctx.log(
                    crate::model::event::LogLevel::Warn,
                    "remote",
                    format!(
                        "[{name}] {} pull op(s) skipped: '{}' declares no |mount=, and the peer lane cannot pull without one (add |mount=<path serving the same tree>)",
                        src_ops.len(),
                        job.target
                    ),
                );
            }
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
