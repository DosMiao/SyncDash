//! Apply evidence preparation and preflight review policy.

pub(super) mod model;
mod preflight;
mod retained;

pub(super) use preflight::{
    apply_facts, apply_review_messages, build_apply_review, require_clean_autoscan_health,
};
pub(super) use retained::{prepare_apply, prepare_autoscan_apply};
