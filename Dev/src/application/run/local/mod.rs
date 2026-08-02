//! In-process run orchestration.
//!
//! A local run is defined by who executes it, not by the root protocol: mounted paths and VFS
//! roots are handled here, while `peer://` work belongs to the peer lane. The tree separates the
//! read-only Compare pipeline, the guarded Apply boundary, and the CLI's multi-target execution
//! loop so that safety policy is not interleaved with presentation and iteration.

mod apply;
mod compare;
mod execute;

pub use apply::{
    apply_job_guarded_with, apply_job_guarded_with_consent,
    apply_job_guarded_with_consent_classified, apply_requirements, apply_requirements_resolved,
    apply_resolved, apply_resolved_with_consent, apply_resolved_with_consent_classified,
    preflight_job, preflight_resolved,
};
pub use compare::{
    compare_capabilities, compare_job_detailed, compare_job_detailed_with_consent,
    compare_resolved, compare_resolved_with_consent,
};
pub use execute::{run_local_job, run_local_single};

#[cfg(test)]
mod tests;
