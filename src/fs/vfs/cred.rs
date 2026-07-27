//! Credentials live in the OS store (Windows Credential Manager / macOS Keychain),
//! never in a job file, never in a phrase. The phrase *derives* the account key, so
//! there is nothing to store per job and nothing that can leak through git, sync,
//! logs or table headers. FFS's `|pass64=` (base64 in the config) was considered and
//! refused: it looks like encryption without being any, and job files travel.
//!
//! The OS stores cannot enumerate our entries, so `set`/`rm` maintain a plain-text
//! *account index* (names only, zero secrets) under the config dir — that is what
//! `syncdash cred ls` lists.

use std::io::Write as _;
use std::sync::Arc;

use super::error::{VfsError, VfsErrorKind, VfsResult};
use super::spec::{default_port, RemoteSpec};
use super::{CredentialProvider, Credentials};

const SERVICE: &str = "syncdash";

/// The keyring account for a remote spec: `{scheme}://{user}@{host}:{port}` with the
/// host folded to lowercase and the port always explicit — same canonicalization as
/// `RemoteSpec::identity`, so one server is one entry no matter how the phrase spells it.
/// None when the phrase names no user: nothing to look up, and never a guessed account.
pub fn account_for(spec: &RemoteSpec) -> Option<String> {
    let user = spec.user.as_deref()?;
    Some(format!(
        "{}://{}@{}:{}",
        spec.scheme,
        user,
        spec.host.to_lowercase(),
        spec.port.unwrap_or(default_port(&spec.scheme))
    ))
}

/// The passphrase entry for an SFTP private key, keyed by the key file's path digest
/// (two different keys must never share a passphrase entry).
pub fn passphrase_account(keyfile: &std::path::Path) -> String {
    let h = blake3::hash(keyfile.to_string_lossy().as_bytes());
    format!("sftp-pass://{}", &h.to_hex()[..16])
}

fn entry(account: &str) -> VfsResult<keyring::Entry> {
    keyring::Entry::new(SERVICE, account).map_err(|e| {
        VfsError::new(VfsErrorKind::Io, format!("credential store unavailable: {e}"))
    })
}

/// Ok(None) = the store has no entry for this account (a normal state, the caller
/// decides whether that is an error); Err = the store itself failed.
pub fn get_secret(account: &str) -> VfsResult<Option<String>> {
    match entry(account)?.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(VfsError::new(
            VfsErrorKind::Io,
            format!("credential store read failed for '{account}': {e}"),
        )),
    }
}

pub fn set_secret(account: &str, secret: &str) -> VfsResult<()> {
    entry(account)?
        .set_password(secret)
        .map_err(|e| VfsError::new(VfsErrorKind::Io, format!("credential store write failed for '{account}': {e}")))?;
    index_add(account);
    Ok(())
}

/// Returns whether an entry existed.
pub fn delete_secret(account: &str) -> VfsResult<bool> {
    let existed = match entry(account)?.delete_credential() {
        Ok(()) => true,
        Err(keyring::Error::NoEntry) => false,
        Err(e) => {
            return Err(VfsError::new(
                VfsErrorKind::Io,
                format!("credential store delete failed for '{account}': {e}"),
            ))
        }
    };
    index_remove(account);
    Ok(existed)
}

// ---- the names-only index (what `cred ls` shows) ----

fn index_file() -> std::path::PathBuf {
    crate::foundation::dirs::config_dir().join("cred-index.txt")
}

pub fn list_accounts() -> Vec<String> {
    std::fs::read_to_string(index_file())
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

fn write_index(mut accounts: Vec<String>) {
    accounts.sort();
    accounts.dedup();
    let file = index_file();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::File::create(&file) {
        for a in accounts {
            let _ = writeln!(f, "{a}");
        }
    }
}

fn index_add(account: &str) {
    let mut v = list_accounts();
    v.push(account.to_string());
    write_index(v);
}

fn index_remove(account: &str) {
    let v: Vec<String> = list_accounts().into_iter().filter(|a| a != account).collect();
    write_index(v);
}

// ---- the engine-side provider ----

/// Keyring-backed, prompt-free. What the engine lanes use: a phrase's own options
/// (keyfile, agent) pass through, the password comes from the OS store when present.
/// It never prompts and never invents — a backend that still lacks a required secret
/// raises `Auth` naming `syncdash cred set "<phrase>"` as the remedy, which is exactly
/// the behaviour a headless/watch run needs.
pub struct KeyringProvider;

impl CredentialProvider for KeyringProvider {
    fn credentials_for(&self, spec: &RemoteSpec) -> VfsResult<Credentials> {
        let keyfile = spec.opt("key").map(expand_tilde);
        let password = match account_for(spec) {
            Some(acc) => get_secret(&acc)?,
            None => None,
        };
        let passphrase = match &keyfile {
            Some(k) => get_secret(&passphrase_account(k))?,
            None => None,
        };
        Ok(Credentials {
            password,
            keyfile,
            passphrase,
            use_agent: spec.has_flag("agent"),
            anonymous: spec.user.as_deref() == Some("anonymous"),
        })
    }
}

pub fn default_provider() -> Arc<dyn CredentialProvider> {
    Arc::new(KeyringProvider)
}

/// `~/x` in a phrase option means the *local* home (keyfiles live on this machine).
fn expand_tilde(s: &str) -> std::path::PathBuf {
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::spec::{parse, RootSpec};

    fn spec(s: &str) -> RemoteSpec {
        match parse(s) {
            RootSpec::Remote(r) => r,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn account_canonicalizes_like_identity() {
        let a = account_for(&spec("sftp://ben@Mac-Mini/Code")).unwrap();
        let b = account_for(&spec("sftp://ben@mac-mini:22/other/root")).unwrap();
        assert_eq!(a, b, "same server+user = same entry, root plays no part");
        assert_eq!(a, "sftp://ben@mac-mini:22");
        // no user, no account — never a guessed one
        assert!(account_for(&spec("ftp://nas/pub")).is_none());
    }

    #[test]
    fn passphrase_accounts_differ_per_keyfile() {
        let a = passphrase_account(std::path::Path::new("C:/keys/a"));
        let b = passphrase_account(std::path::Path::new("C:/keys/b"));
        assert_ne!(a, b);
        assert!(a.starts_with("sftp-pass://"));
    }

    /// Touches the real OS credential store — run explicitly with `--ignored` once per machine.
    #[test]
    #[ignore]
    fn os_store_roundtrip() {
        let acc = "test://syncdash-selftest@localhost:1";
        set_secret(acc, "s3cret").unwrap();
        assert_eq!(get_secret(acc).unwrap().as_deref(), Some("s3cret"));
        assert!(list_accounts().iter().any(|a| a == acc));
        assert!(delete_secret(acc).unwrap());
        assert_eq!(get_secret(acc).unwrap(), None);
        assert!(!list_accounts().iter().any(|a| a == acc));
    }
}
