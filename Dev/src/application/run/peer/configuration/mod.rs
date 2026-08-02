//! Peer phrase configuration and the remote scan command derived from a job.

mod scan;
mod settings;

pub(super) use scan::build_peer_scan_arguments;
#[cfg(test)]
pub(super) use settings::absolute_peer_root;
pub(super) use settings::parse_peer_link_settings;
