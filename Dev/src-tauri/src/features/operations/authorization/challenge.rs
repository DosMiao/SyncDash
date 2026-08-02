//! One-use review challenge, approval, and issued-token vocabulary.

use super::apply::ApplyReview;
use super::compare::CompareAuthorization;

#[derive(Clone, Debug)]
pub(crate) enum ReviewChallenge {
    Compare {
        authorization: CompareAuthorization,
        requires_capability_ack: bool,
    },
    InteractiveApply {
        review: ApplyReview,
        requires_health_ack: bool,
        requires_capability_ack: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ReviewApproval {
    Compare {
        accept_capabilities: bool,
        remember_for_session: bool,
    },
    InteractiveApply {
        acknowledge_health: bool,
        accept_capabilities: bool,
        session_grant: ApplySessionGrantDecision,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplySessionGrantDecision {
    None,
    RememberCapabilities,
    AllowAutoApply,
}

#[derive(Clone, Debug)]
pub(crate) struct IssuedChallenge {
    pub(crate) challenge_id: String,
    pub(crate) expires_at_ms: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct IssuedAuthorization {
    pub(crate) authorization_token: String,
    pub(crate) expires_at_ms: u64,
}
