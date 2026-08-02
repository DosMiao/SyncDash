//! Immutable retained Compare artifacts and presentation views.

use std::sync::Arc;

use crate::contracts::compare::{
    CompareIdentity, CompareOwner, CompareWorkspaceSnapshotDto, PlanDto,
};

pub(in crate::features::compare::evidence) struct RetainedPlan {
    pub(in crate::features::compare::evidence) header: syncdash::model::plan::PlanHeader,
    pub(in crate::features::compare::evidence) operations: Vec<syncdash::model::plan::Op>,
    pub(in crate::features::compare::evidence) metadata:
        Vec<Option<syncdash::pipeline::compare::evidence::RowMeta>>,
    pub(in crate::features::compare::evidence) identical_count: u64,
    pub(in crate::features::compare::evidence) identical_bytes: u64,
}

pub(crate) struct SuccessfulCompareResult {
    pub(in crate::features::compare::evidence) owner: CompareOwner,
    pub(in crate::features::compare::evidence) plan_digest: String,
    pub(in crate::features::compare::evidence) plan: RetainedPlan,
    pub(in crate::features::compare::evidence) source: syncdash::model::table::TableArtifact,
    pub(in crate::features::compare::evidence) target: syncdash::model::table::TableArtifact,
    pub(in crate::features::compare::evidence) compare_options:
        syncdash::pipeline::compare::CompareOptions,
}

pub(crate) struct SuccessfulComparePublication {
    pub(crate) workspace: CompareWorkspaceSnapshotDto,
}

impl SuccessfulCompareResult {
    pub(crate) fn from_plan(
        plan_digest: String,
        plan: PlanDto,
        source: syncdash::model::table::TableArtifact,
        target: syncdash::model::table::TableArtifact,
        compare_options: syncdash::pipeline::compare::CompareOptions,
    ) -> Self {
        let PlanDto {
            header,
            ops,
            metas,
            identical_count,
            identical_bytes,
            owner,
        } = plan;
        Self {
            owner,
            plan_digest,
            plan: RetainedPlan {
                header,
                operations: ops,
                metadata: metas,
                identical_count,
                identical_bytes,
            },
            source,
            target,
            compare_options,
        }
    }

    pub(crate) fn owner(&self) -> &CompareOwner {
        &self.owner
    }

    pub(in crate::features::compare::evidence) fn into_version(
        self,
    ) -> (CompareResultVersion, String) {
        (
            CompareResultVersion {
                identity: self.owner.identity,
                plan_digest: self.plan_digest,
                plan: self.plan,
                source: self.source,
                target: self.target,
                compare_options: self.compare_options,
            },
            self.owner.job_name,
        )
    }
}

pub(in crate::features::compare::evidence) struct CompareResultVersion {
    pub(in crate::features::compare::evidence) identity: CompareIdentity,
    pub(in crate::features::compare::evidence) plan_digest: String,
    pub(in crate::features::compare::evidence) plan: RetainedPlan,
    pub(in crate::features::compare::evidence) source: syncdash::model::table::TableArtifact,
    pub(in crate::features::compare::evidence) target: syncdash::model::table::TableArtifact,
    pub(in crate::features::compare::evidence) compare_options:
        syncdash::pipeline::compare::CompareOptions,
}

pub(crate) struct RetainedCompareResult {
    pub(in crate::features::compare::evidence) version: Arc<CompareResultVersion>,
    pub(in crate::features::compare::evidence) owner: CompareOwner,
}

impl RetainedCompareResult {
    pub(crate) fn identity(&self) -> &CompareIdentity {
        &self.version.identity
    }

    pub(crate) fn owner(&self) -> &CompareOwner {
        &self.owner
    }

    pub(crate) fn plan_digest(&self) -> &str {
        &self.version.plan_digest
    }

    pub(crate) fn plan(&self) -> PlanDto {
        PlanDto {
            header: self.version.plan.header.clone(),
            ops: self.version.plan.operations.clone(),
            metas: self.version.plan.metadata.clone(),
            identical_count: self.version.plan.identical_count,
            identical_bytes: self.version.plan.identical_bytes,
            owner: self.owner.clone(),
        }
    }

    pub(crate) fn plan_header(&self) -> &syncdash::model::plan::PlanHeader {
        &self.version.plan.header
    }

    pub(crate) fn plan_operations(&self) -> &[syncdash::model::plan::Op] {
        &self.version.plan.operations
    }

    pub(crate) fn plan_metadata(
        &self,
    ) -> &[Option<syncdash::pipeline::compare::evidence::RowMeta>] {
        &self.version.plan.metadata
    }

    pub(crate) fn source(&self) -> &syncdash::model::table::TableArtifact {
        &self.version.source
    }

    pub(crate) fn target(&self) -> &syncdash::model::table::TableArtifact {
        &self.version.target
    }

    pub(crate) fn compare_options(&self) -> &syncdash::pipeline::compare::CompareOptions {
        &self.version.compare_options
    }
}
