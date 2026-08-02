//! Job application subsystem.
//!
//! One TOML file represents one registered job. The private tree separates the persisted domain
//! model, validation and runtime policy, stable revision identity, and migration-aware atomic
//! persistence. This façade keeps `syncdash::job` as the deliberate public boundary.
//!
//! `territory` generates jobs by walking `.ffs-sync` markers; `junk` and `rigor` are explicit
//! configuration vocabularies shared by the CLI and desktop editor.

pub mod junk;
pub mod rigor;
pub mod territory;

mod model;
mod persistence;
mod policy;
mod revision;

pub use model::{Job, SingleTargetJob, SCHEMA};
pub use persistence::{
    delete_job, file_schema, file_schema_named, load, load_all, load_by_id, load_named,
    resolve_path, save_job, swap_job_roots, update_job_root, DeletedJob, JobMutationEffect,
    JobRootField, JobRootMutation, SavedJob, SAMPLE,
};
pub use policy::validate_root_pair;
pub use revision::config_revision;
