use crate::job::SingleTargetJob;
use crate::model::plan::Op;

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
            severity: CapSeverity::Unavailable,
            requested: "a .syncdash-root marker verified before writing".into(),
            actual: "the current peer package protocol cannot inspect the peer marker".into(),
            effect: "the far-side marker is not checked this run — a target that is not mounted looks the same as an empty one".into(),
        });
    }
    if configuration.min_free_pct > 0.0 {
        report.items.push(CapItem {
            feature: "min_free_pct".into(),
            side: "target".into(),
            severity: CapSeverity::Degraded,
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
