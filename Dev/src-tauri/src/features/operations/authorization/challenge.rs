//! One-use review challenge, approval, and issued-token vocabulary.

use super::apply::ApplyReview;

#[derive(Clone, Debug)]
pub(crate) enum ReviewChallenge {
    InteractiveApply { review: ApplyReview },
}

/// The approval carries no choices: the review panel presents evidence, not conditions. It exists
/// so an approval names the one challenge it answers, which is what binds a token to the exact
/// reviewed plan.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ReviewApproval {
    InteractiveApply,
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
