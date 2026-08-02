use crate::job::SingleTargetJob;
use crate::model::plan::Op;

/// Represent unobservable peer-side safeguards as structured review data.
pub fn apply_capabilities(
    job: &SingleTargetJob,
    ops: &[Op],
) -> crate::pipeline::guard::caps::CapReport {
    use crate::model::plan::{Action, Side};
    use crate::pipeline::guard::caps::{CapItem, CapReport, CapSeverity};

    let writes_target = ops
        .iter()
        .any(|op| op.side == Side::Target && !matches!(op.action, Action::Conflict | Action::Note));
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
