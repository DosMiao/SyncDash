use crate::job::SingleTargetJob;
use crate::model::plan::{Op, Plan};

/// Prove deletion-share and peer-mount requirements before any write can start.
pub fn preflight_peer_job(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
) -> crate::pipeline::guard::Verdict {
    let g = job.configuration().guards();
    let st = crate::pipeline::guard::stats::stat_plan(ops);
    let mut gv = crate::pipeline::guard::Verdict::default();
    crate::pipeline::guard::ratio::check_delete_ratio(
        "target",
        &st.target,
        plan.header.target_entries,
        &g,
        &mut gv,
    );
    crate::pipeline::guard::ratio::check_delete_ratio(
        "source",
        &st.source,
        plan.header.source_entries,
        &g,
        &mut gv,
    );
    let needs_pull_mount = ops.iter().any(|op| {
        op.side == crate::model::plan::Side::Source
            && !matches!(
                op.action,
                crate::model::plan::Action::Conflict | crate::model::plan::Action::Note
            )
    });
    if needs_pull_mount {
        match crate::fs::vfs::spec::parse(job.target()) {
            crate::fs::vfs::spec::RootSpec::Endpoint(peer_spec) => {
                match peer_spec.opt("mount").filter(|mount| !mount.is_empty()) {
                    Some(mount) if std::path::Path::new(mount).is_dir() => {}
                    Some(mount) => gv.blockers.push(format!(
                        "source-side actions require the peer mount '{mount}', but it is not an accessible directory"
                    )),
                    None => gv.blockers.push(
                        "source-side actions require |mount=<local path serving the peer tree>; this peer job is push-only without it"
                            .into(),
                    ),
                }
            }
            _ => gv
                .blockers
                .push("the peer target phrase is no longer valid — run Compare again".into()),
        }
    }
    gv
}
