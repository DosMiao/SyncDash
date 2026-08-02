//! Apply review fingerprint and interactive or unattended authorization payloads.

use crate::contracts::compare::{CompareIdentity, ReviewedRowDecisionDto};
use crate::features::autoscan::authority::AutoApplyTicket;

use super::compare::CompareAuthorization;
use super::digest::reviewed_row_decisions_digest;
use super::target::JobTargetRevision;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExactReviewedDecisions {
    reviewed_row_decisions: Vec<ReviewedRowDecisionDto>,
    decisions_digest: String,
}

impl ExactReviewedDecisions {
    pub(crate) fn new(reviewed_row_decisions: Vec<ReviewedRowDecisionDto>) -> Result<Self, String> {
        if reviewed_row_decisions.is_empty() {
            return Err("An Apply authorization must bind at least one included difference".into());
        }
        let decisions_digest = reviewed_row_decisions_digest(&reviewed_row_decisions)?;
        Ok(Self {
            reviewed_row_decisions,
            decisions_digest,
        })
    }

    pub(crate) fn decisions(&self) -> &[ReviewedRowDecisionDto] {
        &self.reviewed_row_decisions
    }

    pub(crate) fn digest(&self) -> &str {
        &self.decisions_digest
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ApplyReview {
    compare_identity: CompareIdentity,
    plan_digest: String,
    reviewed_decisions: ExactReviewedDecisions,
    health_review_digest: String,
    capability_review_digest: String,
}

impl ApplyReview {
    pub(crate) fn new(
        compare_identity: CompareIdentity,
        plan_digest: String,
        reviewed_row_decisions: Vec<ReviewedRowDecisionDto>,
        health_review_digest: String,
        capability_review_digest: String,
    ) -> Result<Self, String> {
        if compare_identity.job_id.is_empty()
            || compare_identity.config_revision.is_empty()
            || plan_digest.is_empty()
            || health_review_digest.is_empty()
            || capability_review_digest.is_empty()
        {
            return Err("The Apply review fingerprint is incomplete".into());
        }
        Ok(Self {
            compare_identity,
            plan_digest,
            reviewed_decisions: ExactReviewedDecisions::new(reviewed_row_decisions)?,
            health_review_digest,
            capability_review_digest,
        })
    }

    pub(crate) fn target(&self) -> JobTargetRevision {
        JobTargetRevision::from(&self.compare_identity)
    }

    pub(crate) fn compare_identity(&self) -> &CompareIdentity {
        &self.compare_identity
    }

    pub(crate) fn reviewed_row_decisions(&self) -> &[ReviewedRowDecisionDto] {
        self.reviewed_decisions.decisions()
    }

    pub(crate) fn capability_review_digest(&self) -> &str {
        &self.capability_review_digest
    }

    pub(crate) fn verify_current(&self, current: &Self) -> Result<(), String> {
        if self.compare_identity != current.compare_identity {
            return Err("The authorized Compare result changed — review Apply again".into());
        }
        if self.plan_digest != current.plan_digest {
            return Err("The authorized plan changed — review Apply again".into());
        }
        if self.reviewed_decisions.digest() != current.reviewed_decisions.digest() {
            return Err("The authorized reviewed decisions changed — review Apply again".into());
        }
        if self.health_review_digest != current.health_review_digest {
            return Err("The plan health report changed — review Apply again".into());
        }
        if self.capability_review_digest != current.capability_review_digest {
            return Err("The backend capability report changed — review Apply again".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InteractiveApplyAuthorization {
    review: ApplyReview,
    health_warning_acknowledged: bool,
}

impl InteractiveApplyAuthorization {
    pub(super) fn new(review: ApplyReview, health_warning_acknowledged: bool) -> Self {
        Self {
            review,
            health_warning_acknowledged,
        }
    }

    pub(crate) fn review(&self) -> &ApplyReview {
        &self.review
    }

    pub(crate) fn health_warning_acknowledged(&self) -> bool {
        self.health_warning_acknowledged
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AutoApplyAuthorization {
    review: ApplyReview,
    ticket: AutoApplyTicket,
}

impl AutoApplyAuthorization {
    pub(super) fn new(review: ApplyReview, ticket: AutoApplyTicket) -> Result<Self, String> {
        if ticket.compare_identity() != &review.compare_identity {
            return Err("The AutoScan ticket does not match its Apply review".into());
        }
        Ok(Self { review, ticket })
    }

    pub(crate) fn review(&self) -> &ApplyReview {
        &self.review
    }

    pub(crate) fn ticket(&self) -> &AutoApplyTicket {
        &self.ticket
    }
}

#[derive(Clone, Debug)]
pub(super) enum OperationAuthorization {
    Compare(CompareAuthorization),
    InteractiveApply(InteractiveApplyAuthorization),
    AutoApply(AutoApplyAuthorization),
}

#[derive(Clone, Debug)]
pub(crate) enum ApplyAuthorization {
    Interactive(InteractiveApplyAuthorization),
    AutoScan(AutoApplyAuthorization),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplyAuthorizationKind {
    Interactive,
    AutoScan,
}

impl ApplyAuthorization {
    pub(crate) fn kind(&self) -> ApplyAuthorizationKind {
        match self {
            Self::Interactive(_) => ApplyAuthorizationKind::Interactive,
            Self::AutoScan(_) => ApplyAuthorizationKind::AutoScan,
        }
    }

    pub(crate) fn review(&self) -> &ApplyReview {
        match self {
            Self::Interactive(authorization) => authorization.review(),
            Self::AutoScan(authorization) => authorization.review(),
        }
    }

    pub(crate) fn health_warning_acknowledged(&self) -> bool {
        match self {
            Self::Interactive(authorization) => authorization.health_warning_acknowledged(),
            Self::AutoScan(_) => false,
        }
    }
}
