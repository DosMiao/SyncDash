//! macOS: find an existing mount for the share, or create one under a private mount point.
//!
//! `mount_smbfs -N` first; when that cannot prompt for credentials, fall back to osascript, which
//! can put the system's own dialog in front of the user.

use std::path::PathBuf;

use crate::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use crate::fs::vfs::spec::RemoteSpec;
use crate::fs::vfs::CredentialProvider;

/// Order of preference: an existing smbfs mount of this share (Finder's or ours) →
/// `mount_smbfs -N` into a private mount point (password from the login Keychain —
/// tick "remember password" on the first Finder connect) → `osascript mount volume`
/// as the GUI-prompt fallback. The password from our own store is *not* handed to
/// mount_smbfs: it would have to travel through argv, visible to every process.
pub fn resolve(
    spec: &RemoteSpec,
    share: &str,
    sub: &str,
    _creds: &dyn CredentialProvider,
) -> VfsResult<PathBuf> {
    if let Some(mp) = find_existing_mount(&spec.host, share, spec.user.as_deref()) {
        return Ok(join_sub(&mp, sub));
    }

    // Private mount point, reused across runs, cleaned by `syncdash net umount`
    let mp = private_mount_dir().join(format!("{}-{}", spec.host.to_lowercase(), share.to_lowercase()));
    std::fs::create_dir_all(&mp)
        .map_err(|e| VfsError::new(VfsErrorKind::Io, format!("cannot create mount point '{}': {e}", mp.display())))?;

    let url = match &spec.user {
        Some(u) => format!("//{u}@{}/{share}", spec.host),
        None => format!("//{}/{share}", spec.host),
    };
    let out = std::process::Command::new("mount_smbfs").arg("-N").arg(&url).arg(&mp).output();
    match out {
        Ok(o) if o.status.success() => return Ok(join_sub(&mp, sub)),
        Ok(o) => {
            let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
            // Authentication failure → one GUI attempt via Finder's own dialog
            if msg.contains("Authentication error") || msg.contains("Permission denied") {
                if osascript_mount(&spec.host, share).is_ok() {
                    if let Some(mp2) = find_existing_mount(&spec.host, share, spec.user.as_deref()) {
                        return Ok(join_sub(&mp2, sub));
                    }
                }
                return Err(VfsError::new(
                    VfsErrorKind::Auth,
                    format!(
                        "mount_smbfs was refused for {url} ({msg}) — connect once in Finder with 'remember password' ticked, then retry"
                    ),
                ));
            }
            return Err(VfsError::new(
                VfsErrorKind::Transient,
                format!("mount_smbfs {url} failed: {msg}"),
            ));
        }
        Err(e) => {
            return Err(VfsError::new(VfsErrorKind::Io, format!("cannot run mount_smbfs: {e}")))
        }
    }
}

fn join_sub(mp: &std::path::Path, sub: &str) -> PathBuf {
    if sub.is_empty() {
        mp.to_path_buf()
    } else {
        crate::foundation::path::join_native(mp, sub)
    }
}

pub fn private_mount_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join("Library").join("Caches").join("syncdash").join("mnt")
}

/// Scan the mount table for an smbfs mount of //[user@]host…/share.
fn find_existing_mount(host: &str, share: &str, want_user: Option<&str>) -> Option<PathBuf> {
    let out = std::process::Command::new("mount").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if !line.contains("(smbfs") {
            continue;
        }
        // "//ben@nas._smb._tcp.local/share on /Volumes/share (smbfs, ...)"
        let Some(rest) = line.strip_prefix("//") else { continue };
        let Some((source, after)) = rest.split_once(" on ") else { continue };
        let mountpoint = after.split(" (").next().unwrap_or("").trim();
        let (muser, hostshare) = match source.split_once('@') {
            Some((u, hs)) => (Some(u), hs),
            None => (None, source),
        };
        let Some((mhost, mshare)) = hostshare.split_once('/') else { continue };
        // Bonjour spelling: "nas._smb._tcp.local" — compare on the first label
        let mhost_base = mhost.split("._smb").next().unwrap_or(mhost);
        let host_base = host.split('.').next().unwrap_or(host);
        if !mhost_base.eq_ignore_ascii_case(host_base) && !mhost.eq_ignore_ascii_case(host) {
            continue;
        }
        if !mshare.eq_ignore_ascii_case(share) {
            continue;
        }
        if let (Some(want), Some(got)) = (want_user, muser) {
            if !got.eq_ignore_ascii_case(want) {
                continue; // mounted, but as somebody else — not ours to claim
            }
        }
        if !mountpoint.is_empty() {
            return Some(PathBuf::from(mountpoint));
        }
    }
    None
}

fn osascript_mount(host: &str, share: &str) -> std::io::Result<()> {
    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!("mount volume \"smb://{host}/{share}\""))
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "osascript mount refused"))
    }
}