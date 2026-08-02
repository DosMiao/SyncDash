//! Compare/Apply safety services and run-lifecycle state machines.

pub(crate) mod apply;
pub(crate) mod authorization;
pub(crate) mod compare;
pub(crate) mod decisions;
pub(crate) mod events;
pub(crate) mod execution;
pub(crate) mod lifecycle;
mod projection;
mod target;
