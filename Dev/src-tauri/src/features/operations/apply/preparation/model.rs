use syncdash::model::plan::{Op, Plan};
use syncdash::pipeline::guard::caps::CapReport;
use syncdash::pipeline::guard::Verdict;

use crate::contracts::compare::{CompareOwner, ReviewedRowDecisionDto};

use crate::features::operations::target::ResolvedJobTarget;

pub(in crate::features::operations::apply) struct PreparedApply {
    pub(in crate::features::operations::apply) target: ResolvedJobTarget,
    pub(in crate::features::operations::apply) owner: CompareOwner,
    pub(in crate::features::operations::apply) plan: Plan,
    pub(in crate::features::operations::apply) plan_digest: String,
    pub(in crate::features::operations::apply) reviewed_row_decisions: Vec<ReviewedRowDecisionDto>,
    pub(in crate::features::operations::apply) reviewed_operations: Vec<Op>,
}

pub(in crate::features::operations::apply) struct RetainedApplyPlan {
    pub(in crate::features::operations::apply) target: ResolvedJobTarget,
    pub(in crate::features::operations::apply) owner: CompareOwner,
    pub(in crate::features::operations::apply) plan: Plan,
    pub(in crate::features::operations::apply) plan_digest: String,
}

pub(in crate::features::operations::apply) struct ApplyFacts {
    pub(in crate::features::operations::apply) unacknowledged: Verdict,
    pub(in crate::features::operations::apply) acknowledged: Verdict,
    pub(in crate::features::operations::apply) capabilities: CapReport,
}
