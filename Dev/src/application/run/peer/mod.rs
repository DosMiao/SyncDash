//! Peer-backed job orchestration.
//!
//! The far side runs SyncDash against its own disk: this process probes the peer, asks it to scan,
//! compares the returned snapshot, and ships a reviewed package for target-side writes. Each stage
//! owns its protocol details below this facade; callers continue to use the stable `run::peer`
//! surface.

mod apply;
mod compare;
mod configuration;
mod controller;
mod link;
mod package;
mod probe;

#[cfg(test)]
mod tests;

pub use apply::{
    apply_capabilities, apply_peer_job_with, apply_peer_job_with_classified, preflight_peer_job,
};
pub use compare::compare_peer_job_detailed;
pub use controller::{run_peer_job, run_peer_job_with};
pub use link::PeerLink;
pub use probe::probe_peer;
