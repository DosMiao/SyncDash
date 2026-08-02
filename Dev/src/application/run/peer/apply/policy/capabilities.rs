use crate::job::SingleTargetJob;
use crate::model::plan::Op;

/// Refuse a peer Apply whose unobservable safeguards were never accepted.
///
/// This gate belongs to the lane, not to the caller. It used to sit in the transport router, which
/// meant only the router's own entry point ran it: `run_job` reaches the peer lane through
/// `run_peer_job`, so a CLI `--apply` against a `peer://` target executed with no capability check
/// at all, and `--accept-caps` was dropped on the way. A gate that a second entry point can walk
/// past is not a gate.
pub(in crate::run::peer) fn enforce_apply_capabilities(
    job: &SingleTargetJob,
    ops: &[Op],
    consent: &crate::pipeline::guard::caps::CapabilityConsent,
) -> std::io::Result<()> {
    let capabilities = apply_capabilities(job, ops);
    let blockers = capabilities.blockers();
    if !blockers.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            blockers
                .iter()
                .map(|item| item.render())
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    if !capabilities.consent_satisfied(
        crate::pipeline::guard::caps::CapabilityScope::ApplyWrite,
        consent,
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "peer apply capability limitations require exact review or --accept-caps",
        ));
    }
    Ok(())
}

/// Represent unobservable peer-side safeguards as structured review data.
pub fn apply_capabilities(
    job: &SingleTargetJob,
    ops: &[Op],
) -> crate::pipeline::guard::caps::CapReport {
    use crate::model::plan::Side;
    use crate::pipeline::guard::caps::{CapItem, CapReport, CapSeverity};

    let writes_target = ops
        .iter()
        .any(|op| op.side == Side::Target && op.action.is_executable());
    if !writes_target {
        return CapReport::default();
    }
    let mut report = CapReport::default();
    let configuration = job.configuration();
    if configuration.require_marker {
        report.items.push(CapItem {
            feature: "require_marker".into(),
            side: "target".into(),
            severity: CapSeverity::Block,
            requested: "a .syncdash-root marker verified before writing".into(),
            actual: "the current peer package protocol cannot inspect the peer marker".into(),
            effect:
                "the required mount-point gate cannot be proven, so target-side writes are refused"
                    .into(),
        });
    }
    if configuration.min_free_pct > 0.0 {
        report.items.push(CapItem {
            feature: "min_free_pct".into(),
            side: "target".into(),
            severity: CapSeverity::NeedsAck,
            requested: format!(
                "at least {:.2}% free space retained before writing",
                configuration.min_free_pct * 100.0
            ),
            actual: "peer free space is not observable through the current package protocol"
                .into(),
            effect: "the peer target can run out of space; staged writes still fail per file without publishing partial content"
                .into(),
        });
    }
    report
}
