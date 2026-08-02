//! Process-wide lifecycle for run preparation, progress-launch reservation, and engine control.

mod control;
pub(crate) mod coordinator;
pub(crate) mod lease;
pub(crate) mod model;
mod preparation;
mod reservation;
mod state;

#[cfg(test)]
mod tests;
