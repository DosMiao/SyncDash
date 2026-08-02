//! TOML codec, ordered schema migrations, registered-job lookup, and atomic mutations.
//!
//! The registry serializes all job-directory mutations through one advisory lock. File decoding is
//! separate from registry identity materialization so direct-path CLI reads never mutate a file.

mod codec;
mod migration;
mod mutation;
mod registry;
mod sample;
mod seed;
mod types;

pub use mutation::{delete_job, save_job, swap_job_roots, update_job_root};
pub use registry::{
    file_schema, file_schema_named, load, load_all, load_by_id, load_named, resolve_path,
};
pub use sample::SAMPLE;
pub use seed::{seed_job_file, SeedOutcome};
pub use types::{DeletedJob, JobMutationEffect, JobRootField, JobRootMutation, SavedJob};
