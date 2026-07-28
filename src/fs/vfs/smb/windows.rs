//! Windows: build the UNC path and authenticate through WNetAddConnection2W.
//!
//! The credential handshake goes through mpr.dll rather than a mount, because Windows already
//! speaks SMB natively — a UNC path *is* the filesystem path once the session exists.

use std::path::PathBuf;

use crate::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use crate::fs::vfs::spec::RemoteSpec;
use crate::fs::vfs::CredentialProvider;

/// `smb://[user@]host/share/sub` → `\\host\share\sub`, attaching credentials over
/// WNetAddConnection2W when the plain probe is refused. The password crosses into
/// the API in memory only — never argv, never a temp file.
pub fn resolve(
    spec: &RemoteSpec,
    share: &str,
    sub: &str,
    creds: &dyn CredentialProvider,
) -> VfsResult<PathBuf> {
    let unc_share = format!(r"\\{}\{}", spec.host, share);
    let full = if sub.is_empty() {
        PathBuf::from(&unc_share)
    } else {
        PathBuf::from(format!(r"{unc_share}\{}", crate::foundation::path::to_native(sub)))
    };

    match probe(&full) {
        Ok(()) => return Ok(full),
        Err(ProbeFail::Denied) => {}
        Err(ProbeFail::Hard(e)) => return Err(e),
    }

    // Access denied under the current login: this is the credentialed path.
    let Some(user) = spec.user.clone() else {
        return Err(VfsError::new(
            VfsErrorKind::Auth,
            format!(
                "access to {unc_share} denied under the current Windows login — name a user in the phrase (smb://user@{}/{share}) and store its password with: syncdash cred set \"{}\"",
                spec.host,
                spec.display()
            ),
        ));
    };
    let c = creds.credentials_for(spec)?;
    let Some(password) = c.password else {
        return Err(VfsError::new(
            VfsErrorKind::Auth,
            format!(
                "no stored password for {user}@{} — store it with: syncdash cred set \"{}\"",
                spec.host,
                spec.display()
            ),
        ));
    };

    // Plain username first; a device-local account often needs the HOST\user form,
    // so a logon failure retries once with that spelling. Both attempts are named
    // in the error if neither lands.
    let mut tried = Vec::new();
    for u in [user.clone(), format!(r"{}\{}", spec.host, user)] {
        if u.contains('\\') && user.contains('\\') && tried.len() == 1 {
            break; // the user already carried a domain; no second spelling to try
        }
        match wnet_connect(&unc_share, &u, &password) {
            Ok(()) => {
                return match probe(&full) {
                    Ok(()) => Ok(full),
                    Err(ProbeFail::Denied) => Err(VfsError::new(
                        VfsErrorKind::Auth,
                        format!("{unc_share} accepted the session for '{u}' but still refuses '{}'", full.display()),
                    )),
                    Err(ProbeFail::Hard(e)) => Err(e),
                };
            }
            Err(code @ 1326) | Err(code @ 86) => {
                tried.push(format!("'{u}' (logon failure {code})"));
                continue;
            }
            Err(1219) => {
                return Err(VfsError::new(
                    VfsErrorKind::Protocol,
                    format!(
                        "Windows already holds a session to {} under a different account (error 1219) — drop it first: net use {unc_share} /delete",
                        spec.host
                    ),
                ));
            }
            Err(code @ (53 | 1231 | 1232 | 121 | 1203)) => {
                return Err(VfsError::new(
                    VfsErrorKind::Transient,
                    format!("cannot reach {} (WNet error {code})", spec.host),
                ));
            }
            Err(67) => {
                return Err(VfsError::new(
                    VfsErrorKind::Protocol,
                    format!("{} answered: no share named '{share}' (error 67)", spec.host),
                ));
            }
            Err(code) => {
                return Err(VfsError::new(
                    VfsErrorKind::Io,
                    format!("WNetAddConnection2 to {unc_share} failed with Windows error {code}"),
                ));
            }
        }
    }
    Err(VfsError::new(
        VfsErrorKind::Auth,
        format!(
            "{unc_share} rejected the stored password for {} — update it with: syncdash cred set \"{}\"",
            tried.join(" and "),
            spec.display()
        ),
    ))
}

enum ProbeFail {
    /// Denied under the current session — worth trying credentials.
    Denied,
    Hard(VfsError),
}

fn probe(p: &std::path::Path) -> Result<(), ProbeFail> {
    match std::fs::metadata(p) {
        Ok(_) => Ok(()),
        Err(e) => match e.raw_os_error() {
            Some(5) | Some(1326) | Some(86) => Err(ProbeFail::Denied),
            Some(53) | Some(1231) | Some(1232) | Some(121) | Some(1203) => Err(ProbeFail::Hard(VfsError::new(
                VfsErrorKind::Transient,
                format!("cannot reach '{}': {e}", p.display()),
            ))),
            Some(67) => Err(ProbeFail::Hard(VfsError::new(
                VfsErrorKind::Protocol,
                format!("the server answered: no such share ('{}', error 67)", p.display()),
            ))),
            _ if e.kind() == std::io::ErrorKind::NotFound => Err(ProbeFail::Hard(VfsError::new(
                VfsErrorKind::NotFound,
                format!("the share is reachable but '{}' does not exist on it", p.display()),
            ))),
            _ => Err(ProbeFail::Hard(e.into())),
        },
    }
}

#[repr(C)]
#[allow(non_snake_case)]
struct NETRESOURCEW {
    dwScope: u32,
    dwType: u32,
    dwDisplayType: u32,
    dwUsage: u32,
    lpLocalName: *mut u16,
    lpRemoteName: *mut u16,
    lpComment: *mut u16,
    lpProvider: *mut u16,
}

#[link(name = "mpr")]
extern "system" {
    fn WNetAddConnection2W(
        lpNetResource: *mut NETRESOURCEW,
        lpPassword: *const u16,
        lpUserName: *const u16,
        dwFlags: u32,
    ) -> u32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Device-less session (no drive letter): exactly what `net use \\host\share` does,
/// minus the password-in-argv exposure.
fn wnet_connect(unc_share: &str, user: &str, password: &str) -> Result<(), u32> {
    const RESOURCETYPE_DISK: u32 = 1;
    let mut remote = wide(unc_share);
    let user_w = wide(user);
    let pass_w = wide(password);
    let mut nr = NETRESOURCEW {
        dwScope: 0,
        dwType: RESOURCETYPE_DISK,
        dwDisplayType: 0,
        dwUsage: 0,
        lpLocalName: std::ptr::null_mut(),
        lpRemoteName: remote.as_mut_ptr(),
        lpComment: std::ptr::null_mut(),
        lpProvider: std::ptr::null_mut(),
    };
    let code = unsafe { WNetAddConnection2W(&mut nr, pass_w.as_ptr(), user_w.as_ptr(), 0) };
    if code == 0 {
        Ok(())
    } else {
        Err(code)
    }
}