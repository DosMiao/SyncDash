//! Making an SMB share reachable as a local path, per platform.
//!
//! Three implementations behind one signature: `resolve(spec, share, sub, creds) -> PathBuf`.
//! They share nothing else — Windows builds a UNC path and authenticates through WNet, macOS
//! finds or creates a mount point, and everything else declines — which is exactly why they are
//! cfg-selected siblings rather than branches inside one function.

#[cfg(windows)]
#[path = "windows.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;
#[cfg(not(any(windows, target_os = "macos")))]
#[path = "unsupported.rs"]
mod imp;

pub use imp::*;

/// `syncdash net umount`: release the private mount points this tool created.
/// macOS only — on Windows the sessions are device-less; `net use \\host\share /delete`
/// is the OS's own tool for dropping one.
pub fn umount_private_mounts() -> Vec<(String, Result<(), String>)> {
    #[cfg(target_os = "macos")]
    {
        // Unqualified: this *is* the platform module, and `private_mount_dir` arrives here
        // through `pub use imp::*` above. The call read `platform::private_mount_dir()` while
        // this function still lived in `smb/mod.rs`, where `platform` was a sibling — the split
        // moved the function without moving the path with it, and no Windows build compiles this
        // branch, so it stayed broken until someone built on a Mac.
        let dir = private_mount_dir();
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                let label = p.display().to_string();
                let r = std::process::Command::new("umount").arg(&p).status();
                match r {
                    Ok(s) if s.success() => out.push((label, Ok(()))),
                    Ok(s) => out.push((label, Err(format!("umount exited with {s}")))),
                    Err(err) => out.push((label, Err(err.to_string()))),
                }
            }
        }
        out
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}
