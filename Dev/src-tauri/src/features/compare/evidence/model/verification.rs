//! Verification tickets and terminal outcomes.

use super::scope::CompareScope;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompareVerificationTicket {
    pub(in crate::features::compare::evidence) scope: CompareScope,
    pub(in crate::features::compare::evidence) epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompareVerificationTerminalOutcome {
    Failed { message: String },
    Cancelled,
}
