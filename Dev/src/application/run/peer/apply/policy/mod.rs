//! Locally provable safety gates and explicit peer protocol limitations.

mod capabilities;
mod preflight;

pub use capabilities::apply_capabilities;
pub(in crate::run::peer) use capabilities::enforce_apply_capabilities;
pub use preflight::preflight_peer_job;
