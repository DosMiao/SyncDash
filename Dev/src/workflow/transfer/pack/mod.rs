//! Tar package transfer for target-side plan operations.
//!
//! `format` owns the versioned manifest and complete first-pass validation. `creation` emits the
//! plan, payloads, and manifest in protocol order while fencing source evidence. `application`
//! verifies and stages every payload before delegating mutation to the normal apply transaction.
//! This façade is the stable `syncdash::transfer::pack` boundary.

mod application;
mod creation;
mod format;

pub use application::apply_pack;
pub use creation::pack;
pub(crate) use creation::pack_to_open_file;
pub use format::{Manifest, PackSummary, PayloadEntry, PACK_VERSION};
