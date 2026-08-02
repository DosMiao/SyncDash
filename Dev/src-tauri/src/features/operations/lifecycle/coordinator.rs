//! Shared lifecycle coordinator state.

use std::sync::Mutex;

use super::state::LifecycleState;

#[derive(Default)]
pub(crate) struct RunLifecycle {
    pub(super) state: Mutex<LifecycleState>,
}
