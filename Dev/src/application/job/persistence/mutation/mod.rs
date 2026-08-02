//! Atomic, identity- and revision-fenced mutations of registered jobs.
//!
//! Every entry point here takes the job-directory lock, verifies that the file on disk is still
//! the one the caller reviewed, and publishes through a staged write. The fence is separated from
//! the mutations so that all four share one implementation of "is this still the job you saw".

mod delete;
mod fence;
mod roots;
mod save;
mod seed;

#[cfg(test)]
mod tests;

pub use delete::delete_job;
pub use roots::{swap_job_roots, update_job_root};
pub use save::save_job;
pub use seed::{seed_job_file, SeedOutcome};
