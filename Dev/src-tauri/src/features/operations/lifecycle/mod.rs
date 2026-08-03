//! Process-wide lifecycle for run preparation, progress-launch reservation, and engine control.

mod control;
pub(crate) mod lease;
pub(crate) mod model;
mod preparation;
pub(crate) mod progress_window;
mod reservation;
mod state;

#[cfg(test)]
mod tests;

use std::sync::Mutex;

use self::state::LifecycleState;

/// The single owner of active-run, command-exclusion, and progress-launch state. Every decision
/// that depends on more than one of those fields is taken under this one lock.
#[derive(Default)]
pub(crate) struct RunLifecycle {
    state: Mutex<LifecycleState>,
}
