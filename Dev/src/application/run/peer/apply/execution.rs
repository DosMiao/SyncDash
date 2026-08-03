use crate::job::SingleTargetJob;
use crate::model::plan::{Action, Op, Plan};
use crate::run::{archive::refresh_archive_with, scan_opts};

use super::super::package::TemporaryPeerPackage;
use super::super::probe::probe_peer;
use super::policy::preflight_peer_job;

#[allow(clippy::too_many_arguments)] // reviewed inputs and the write-boundary witness remain explicit
pub(super) fn apply_peer_inner(
    name: &str,
    job: &SingleTargetJob,
    plan_full: &Plan,
    sel_ops: &[Op],
    verbose: bool,
    ctx: &crate::obs::progress::RunCtx,
    writes_started: &mut bool,
) -> std::io::Result<crate::obs::progress::ApplyOutcome> {
    use crate::model::event::{Phase, ProgressEvent};
    use crate::model::plan::Side;
    use crate::obs::progress::{ApplyOutcome, PhaseProgress};

    let configuration = job.configuration();
    let gv = preflight_peer_job(job, plan_full, sel_ops);
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
        return Ok(ApplyOutcome {
            done: 0,
            skipped: sel_ops.len() as u64,
            errors: 1,
            bytes_copied: 0,
            cancelled: false,
        });
    }

    let link = probe_peer(name, job, ctx)?;
    let (host, peer_root, shell) = (link.host.as_str(), link.peer_root.as_str(), link.shell);
    // Packing and the pull-back only look at the finalised subset; the full plan is used only for the archive refresh (dropping conflicted paths needs all of it)
    let plan = Plan {
        header: plan_full.header.clone(),
        ops: sel_ops.to_vec(),
    };

    let mut done = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;
    let mut bytes_done_total = 0u64;

    let has_target_ops = plan
        .ops
        .iter()
        .any(|o| o.side == Side::Target && o.action.is_executable());
    if has_target_ops {
        let delta_rels: Vec<String> = plan
            .ops
            .iter()
            .filter(|o| {
                o.side == Side::Target
                    && o.action == Action::Update
                    && o.link.is_none()
                    && o.size
                        .map(|s| s >= crate::model::chunk::DELTA_MIN_SIZE)
                        .unwrap_or(false)
            })
            .map(|o| o.path.clone())
            .collect();
        let peer_chunks = if delta_rels.is_empty() {
            None
        } else {
            let mut args: Vec<String> =
                vec!["chunks".into(), "--root".into(), peer_root.to_string()];
            for r in &delta_rels {
                args.push("--file".into());
                args.push(r.clone());
            }
            match link.session.capture(&link.command(&args)) {
                Ok(bytes) => {
                    let mut m = std::collections::HashMap::new();
                    for line in String::from_utf8_lossy(&bytes).lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if let Ok(fc) =
                            serde_json::from_str::<crate::model::chunk::FileChunks>(line)
                        {
                            m.insert(fc.rel.as_str().to_owned(), fc);
                        }
                    }
                    ctx.log(
                        crate::model::event::LogLevel::Info,
                        "delta",
                        format!(
                            "[{name}] delta: got chunk tables for {} large file(s)",
                            m.len()
                        ),
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
        let pp_pack = PhaseProgress::begin(
            ctx,
            Phase::Pack,
            Some("packing target-side content".into()),
            0,
            0,
        );
        let package = TemporaryPeerPackage::create()?;
        let sum = crate::transfer::pack::pack_to_open_file(
            &plan,
            configuration.source_path(),
            package.output()?,
            peer_chunks.as_ref(),
        )?;
        pp_pack.set_totals(sum.ops, sum.bytes);
        if sum.delta_saved > 0 {
            ctx.log(
                crate::model::event::LogLevel::Info,
                "pack",
                format!(
                    "[{name}] packed {} B, delta saved {} B",
                    sum.bytes, sum.delta_saved
                ),
            );
        }
        pp_pack.complete("package");
        pp_pack.finish()?;
        let peer_package_path = if shell == crate::transfer::peer::PeerShell::PowerShell {
            // PowerShell resolves this relative path in the peer account's home directory.
            package.file_name().to_owned()
        } else {
            format!("/tmp/{}", package.file_name())
        };
        ctx.checkpoint()?;
        let tar_len = package.len()?;
        let pp_ship =
            PhaseProgress::begin(ctx, Phase::Ship, Some(format!("→ ssh:{host}")), 1, tar_len);
        let recv_cmd = link.command(&["recv".into(), peer_package_path.clone()]);
        // Bytes are counted as they leave, not assumed once the transfer returns: the old
        // transport handed the file to a child process and learned nothing until it exited, so a
        // multi-gigabyte package sat at 0% for its whole duration.
        link.session
            .send_file(&recv_cmd, package.reader()?, &mut |n| {
                pp_ship.add_bytes(n, &peer_package_path);
                // Cancel now stops the upload instead of letting it run to completion unwatched
                ctx.checkpoint()
            })?;
        pp_ship.item_done(&peer_package_path);
        pp_ship.finish()?;
        bytes_done_total += sum.bytes;

        ctx.checkpoint()?;
        let pp_ra = PhaseProgress::begin(
            ctx,
            Phase::Apply,
            Some(format!("ssh:{host} apply-pack")),
            sum.ops,
            0,
        );
        let mut ap_args: Vec<String> = vec![
            "apply-pack".into(),
            peer_package_path.clone(),
            "--apply".into(),
            "--remove-pkg".into(),
        ];
        if configuration.versioning {
            ap_args.push("--versioning".into());
        }
        if verbose {
            ap_args.push("-v".into());
        }
        // Once this command is handed to ssh, a transport error cannot prove whether the far side
        // started publishing files. Mark the boundary before invocation, not after its response.
        *writes_started = true;
        let ok = link.session.run_status(&link.command(&ap_args))?;
        if ok {
            done += sum.ops;
            pp_ra.complete(&peer_package_path);
            if pp_ra.finish().is_err() {
                return Ok(ApplyOutcome {
                    done,
                    skipped,
                    errors,
                    bytes_copied: bytes_done_total,
                    cancelled: true,
                });
            }
        } else {
            errors += 1;
            ctx.log(
                crate::model::event::LogLevel::Error,
                "peer",
                format!("[{name}] peer apply-pack reported failure"),
            );
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::foundation::time::now_ms(),
                path: peer_package_path.clone(),
                action: "apply-pack".into(),
                side: "target".into(),
                message: "peer apply-pack reported failure".into(),
            });
            pp_ra.fail();
        }
    }

    // The peer lane only pushes — it packs ops for the far
    // side to apply — so a pull has to read the peer tree through a mount of it, named by
    // `|mount=` on the phrase. No mount means the job never had a pull path; say so plainly
    // rather than reporting ops as skipped for a reason the config does not mention.
    let src_ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| o.side == Side::Source && o.action.is_executable())
        .cloned()
        .collect();
    if !src_ops.is_empty() {
        match link.mount.as_deref() {
            Some(m) if m.is_dir() => {
                *writes_started = true;
                let out = crate::pipeline::apply::apply_with(
                    &src_ops,
                    configuration.source_path(),
                    m,
                    &configuration.apply_opts(None, verbose),
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
                    "peer",
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
                    "peer",
                    format!(
                        "[{name}] {} pull op(s) skipped: '{}' declares no |mount=, and the peer lane cannot pull without one (add |mount=<path serving the same tree>)",
                        src_ops.len(),
                        job.target()
                    ),
                );
            }
        }
    }

    if errors == 0 && !ctx.ctl.cancelled() && configuration.mode == "sync" {
        // A peer job's source is a root this process owns, so opening it is local work, not a
        // second handshake. A failure here used to return silently; an archive that did not get
        // refreshed changes what the next run concludes, so it says so.
        match crate::run::roots::resolve_root(&configuration.source) {
            // The peer lane has no local handle on the far root, so no joint-tier narrowing applies
            // and never did: its comparison runs at the job's own tier, and the archive matches it.
            Ok(sv) => {
                *writes_started = true;
                if !refresh_archive_with(
                    configuration,
                    plan_full,
                    &sv,
                    &scan_opts(configuration),
                    ctx,
                ) && !ctx.ctl.cancelled()
                {
                    errors += 1;
                }
            }
            Err(e) => {
                errors += 1;
                ctx.sink.emit(ProgressEvent::Error {
                    phase: Phase::Refresh,
                    ts_ms: crate::foundation::time::now_ms(),
                    path: configuration.source.clone(),
                    action: "archive".into(),
                    side: "source".into(),
                    message: format!("archive not refreshed — the source root would not open: {e}"),
                });
            }
        }
    }
    Ok(ApplyOutcome {
        done,
        skipped,
        errors,
        bytes_copied: bytes_done_total,
        cancelled: ctx.ctl.cancelled(),
    })
}
