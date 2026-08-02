//! Stable target identity, registry resolution, freshness validation, and authorization binding.

mod authorization;
mod model;
mod resolution;

pub(super) use authorization::build_compare_authorization;
pub(super) use model::ResolvedJobTarget;
pub(super) use resolution::{
    load_bound_target, load_review_target, reload_prepared_target, resolve_job_target,
};
