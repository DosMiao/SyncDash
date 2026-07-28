//! Opening an SFTP session: the authentication chain and the host-key check.
//!
//! Split out because none of it touches `SftpBackend` — `connect_and_open` is a free async fn over
//! a spec and a credential provider, and the host-key types are pure policy. What is left in
//! `mod.rs` is the filesystem, not the handshake.


use crate::fs::vfs::error::{VfsError, VfsErrorKind};

use russh::keys::{known_hosts, HashAlg};

#[derive(Debug)]
pub(super) enum HostCheckError {
    Ssh(russh::Error),
    Unknown { fingerprint: String },
    Changed { fingerprint: String },
}
impl std::fmt::Display for HostCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostCheckError::Ssh(e) => write!(f, "{e}"),
            HostCheckError::Unknown { fingerprint } => write!(f, "unknown host key {fingerprint}"),
            HostCheckError::Changed { fingerprint } => write!(f, "HOST KEY CHANGED to {fingerprint}"),
        }
    }
}
impl std::error::Error for HostCheckError {}
impl From<russh::Error> for HostCheckError {
    fn from(e: russh::Error) -> Self {
        HostCheckError::Ssh(e)
    }
}
/// Verifies against the user's own OpenSSH `~/.ssh/known_hosts` — the trust the ssh
/// smart-peer mode already built up is reused, not duplicated. Unknown host = refuse
/// with the fingerprint and the remedy (connect once with ssh); changed key = refuse
/// loudly, never auto-continue.
pub(super) struct HostCheck {
    pub(super) host: String,
    pub(super) port: u16,
}
impl russh::client::Handler for HostCheck {
    type Error = HostCheckError;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        match known_hosts::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => Ok(true),
            Ok(false) => Err(HostCheckError::Unknown { fingerprint: fp }),
            Err(_) => Err(HostCheckError::Changed { fingerprint: fp }),
        }
    }
}
pub(super) fn map_connect_err(e: HostCheckError, host: &str, display: &str) -> VfsError {
    match e {
        HostCheckError::Changed { fingerprint } => VfsError::new(
            VfsErrorKind::Auth,
            format!(
                "!!! the host key for {host} DOES NOT MATCH ~/.ssh/known_hosts (now {fingerprint}) — possible man-in-the-middle; refusing. If the server was really reinstalled, remove the old line with ssh-keygen -R {host}"
            ),
        ),
        HostCheckError::Unknown { fingerprint } => VfsError::new(
            VfsErrorKind::Auth,
            format!(
                "{host} is not in ~/.ssh/known_hosts (its key is {fingerprint}) — connect once with `ssh {host}` to record it, then retry '{display}'"
            ),
        ),
        HostCheckError::Ssh(e) => {
            VfsError::new(VfsErrorKind::Transient, format!("ssh connection to {host} failed: {e}"))
        }
    }
}
