use syncdash::model::plan::{Op, Plan};
use syncdash::pipeline::compare::evidence::RowMeta;
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
    /// The retained result's per-side row evidence, parallel to `plan.ops`. Reversed rows are
    /// reconstructed from it, so it travels with the plan instead of being re-derived.
    pub(in crate::features::operations::apply) plan_metadata: Vec<Option<RowMeta>>,
    pub(in crate::features::operations::apply) plan_digest: String,
}

pub(in crate::features::operations::apply) struct ApplyFacts {
    pub(in crate::features::operations::apply) verdict: Verdict,
    pub(in crate::features::operations::apply) capabilities: CapReport,
}
