//! One ssh session, shared by everything that rides ssh.
//!
//! Two things in this crate speak ssh and they used to do it by different means: `vfs::sftp` opened
//! a russh session in-process, while `transfer::peer` shelled out to the `ssh` binary and quoted
//! commands for whatever shell the far side runs. Same hosts, same keys, same `known_hosts` — two
//! implementations, one of which needed OpenSSH installed and could not be cancelled or measured.
//!
//! So the handshake lives here once: connect, verify the host key against the user's own
//! `~/.ssh/known_hosts`, and walk the authentication chain. What each caller does with the session
//! afterwards is its own business — sftp requests a subsystem, the peer lane runs commands.
//!
//! **`exec` is not argv.** SSH's exec request (RFC 4254 §6.5) carries a single command string that
//! sshd hands to the user's login shell, so a caller still has to quote for the dialect the far
//! side runs. Being in-process removes the child process, not the shell.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::keys::{known_hosts, load_secret_key, HashAlg, PrivateKeyWithHashAlg};

use crate::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use crate::fs::vfs::Credentials;

#[derive(Debug)]
pub enum HostCheckError {
    Ssh(russh::Error),
    Unknown { fingerprint: String },
    Changed { fingerprint: String },
}

impl std::fmt::Display for HostCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostCheckError::Ssh(e) => write!(f, "{e}"),
            HostCheckError::Unknown { fingerprint } => write!(f, "unknown host key {fingerprint}"),
            HostCheckError::Changed { fingerprint } => {
                write!(f, "HOST KEY CHANGED to {fingerprint}")
            }
        }
    }
}

impl std::error::Error for HostCheckError {}
impl From<russh::Error> for HostCheckError {
    fn from(e: russh::Error) -> Self {
        HostCheckError::Ssh(e)
    }
}

/// Verifies against the user's own OpenSSH `~/.ssh/known_hosts` — the trust an existing `ssh`
/// setup already built up is reused, not duplicated. Unknown host = refuse with the fingerprint
/// and the remedy; changed key = refuse loudly, never auto-continue.
pub struct HostCheck {
    pub host: String,
    pub port: u16,
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

pub fn map_connect_err(e: HostCheckError, host: &str, display: &str) -> VfsError {
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

/// An authenticated session, plus the rungs of the auth chain that were tried.
///
/// The log is returned rather than logged here because its one good use is the *failure* message:
/// "tried: key ~/.ssh/id_ed25519 (server refused); password (none stored)" tells the reader what to
/// fix, where "authentication failed" does not.
pub struct Session {
    pub handle: russh::client::Handle<HostCheck>,
    pub tried: Vec<String>,
}

impl Session {
    /// How the session was authenticated, for a server-info line.
    pub fn auth_summary(&self) -> String {
        self.tried.last().cloned().unwrap_or_default()
    }
}

/// Connect and authenticate. Keys first, most specific first, password last; every failed rung is
/// named in the final error.
pub async fn connect(
    host: &str,
    port: u16,
    user: &str,
    creds: &Credentials,
    timeout: Duration,
    display: &str,
) -> VfsResult<Session> {
    let config = Arc::new(russh::client::Config::default());
    let handler = HostCheck {
        host: host.to_string(),
        port,
    };

    let mut session = match tokio::time::timeout(
        timeout,
        russh::client::connect(config, (host, port), handler),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(map_connect_err(e, host, display)),
        Err(_) => {
            return Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("connecting to {host}:{port} timed out after {timeout:?}"),
            ))
        }
    };

    let mut tried: Vec<String> = Vec::new();
    let mut authed = false;

    let mut key_candidates: Vec<(PathBuf, bool)> = Vec::new(); // (path, explicit)
    if let Some(k) = &creds.keyfile {
        key_candidates.push((k.clone(), true));
    } else if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    {
        let ssh = PathBuf::from(home).join(".ssh");
        for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
            let p = ssh.join(name);
            if p.is_file() {
                key_candidates.push((p, false));
            }
        }
    }
    for (path, explicit) in key_candidates {
        match load_secret_key(&path, creds.passphrase.as_deref()) {
            Ok(key) => {
                let hash = session
                    .best_supported_rsa_hash()
                    .await
                    .ok()
                    .flatten()
                    .flatten();
                let k = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
                match session.authenticate_publickey(user, k).await {
                    Ok(r) if r.success() => {
                        authed = true;
                        tried.push(format!("key {} ✓", path.display()));
                        break;
                    }
                    Ok(_) => tried.push(format!("key {} (server refused)", path.display())),
                    Err(e) => tried.push(format!("key {} ({e})", path.display())),
                }
            }
            Err(e) => {
                let head = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.lines().next().map(|l| l.to_string()))
                    .unwrap_or_default();
                let hint = if head.contains("PuTTY") {
                    " — this is a PuTTY .ppk; export an OpenSSH-format key with puttygen"
                } else if head.starts_with("ssh-") || head.starts_with("ecdsa-") {
                    " — this is a PUBLIC key; point at the private one"
                } else {
                    ""
                };
                let line = format!("key {} unreadable: {e}{hint}", path.display());
                // A key the user named explicitly failing is a configuration error, not a rung to
                // step past — falling through to password auth would obscure what they got wrong.
                if explicit {
                    return Err(VfsError::new(VfsErrorKind::Auth, line));
                }
                tried.push(line);
            }
        }
    }

    if !authed {
        if let Some(pw) = &creds.password {
            match session.authenticate_password(user, pw.clone()).await {
                Ok(r) if r.success() => {
                    authed = true;
                    tried.push("password ✓".into());
                }
                Ok(_) => tried.push("password (server refused)".into()),
                Err(e) => tried.push(format!("password ({e})")),
            }
        } else {
            tried.push("password (none stored — syncdash cred set can add one)".into());
        }
    }

    if !authed {
        return Err(VfsError::new(
            VfsErrorKind::Auth,
            format!(
                "authentication to {user}@{host} failed; tried: {}",
                tried.join("; ")
            ),
        ));
    }

    Ok(Session {
        handle: session,
        tried,
    })
}
