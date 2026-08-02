use crate::job::SingleTargetJob;
use crate::model::table::TableArtifact;
use crate::pipeline::{compare, scan};
use crate::run::{scan_opts, CompareOutcome};

use super::configuration::build_peer_scan_arguments;
use super::probe::probe_peer;

/// Compare a local source snapshot with the complete snapshot returned by the peer.
pub fn compare_peer_job_detailed(
    name: &str,
    job: &SingleTargetJob,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<CompareOutcome> {
    use crate::model::event::Phase;
    use crate::obs::progress::PhaseProgress;
    let configuration = job.configuration();
    let link = probe_peer(name, job, ctx)?;

    let scan_arguments = build_peer_scan_arguments(configuration, &link.peer_root);
    ctx.checkpoint()?;
    // The peer scans on its own disk, so this process has no totals until the table returns.
    let pp_rs = PhaseProgress::begin(
        ctx,
        Phase::ScanTarget,
        Some(format!("ssh:{} {}", link.host, link.peer_root)),
        0,
        0,
    );
    let table_bytes = link.session.capture(&link.command(&scan_arguments))?;
    let t = TableArtifact::read_snapshot(std::io::BufReader::new(&table_bytes[..]))?;
    pp_rs.finish()?;

    let mut v = crate::pipeline::guard::Verdict {
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    crate::pipeline::guard::roots::check_root(
        "source",
        configuration.source_path(),
        configuration.require_marker,
        &mut v,
    );
    for w in &v.warnings {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "compare",
            format!("[{name}] warning: {w}"),
        );
    }
    if !v.ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            v.blockers.join("; "),
        ));
    }
    let options = scan_opts(configuration);
    let s = scan::scan_ctx(
        configuration.source_path(),
        &options,
        ctx,
        Phase::ScanSource,
    )?;
    let archive = crate::run::load_comparison_archive(configuration, &options, ctx)?;
    let pp_cmp = PhaseProgress::begin(
        ctx,
        Phase::Compare,
        Some(format!(
            "{} × {} entries",
            s.header.entry_count, t.header.entry_count
        )),
        0,
        0,
    );
    let copts = configuration.compare_opts();
    let plan = compare::compare(&s, &t, &configuration.mode, archive.as_ref(), false, &copts);
    pp_cmp.finish()?;
    Ok(CompareOutcome {
        plan,
        source: s,
        target: t,
        compare_options: copts,
    })
}
