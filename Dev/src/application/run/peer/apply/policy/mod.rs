//! Locally provable safety gates and explicit peer protocol limitations.

mod capabilities;
mod preflight;

pub use capabilities::apply_capabilities;
pub use preflight::preflight_peer_job;
