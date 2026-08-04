//! The supported host set, declared once.
//!
//! Every platform seam in the crate writes exhaustive arms over these three hosts and carries no
//! "some other OS" fallback; this backstop is what makes that safe. Porting to a fourth host
//! starts here, and the compiler then lists every seam that needs an arm.
//!
//! `HostOs` answers "which mechanism does this build use" — the few legitimate runtime host
//! checks. It never answers what a tree's semantics are: case sensitivity, mtime precision, and
//! their kin are volume capabilities probed per root (`fs::vfs::VfsCaps`), because a root phrase
//! can reach a filesystem with different rules than the host's own.

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
compile_error!(
    "SyncDash supports Windows, macOS, and Linux hosts only; \
     add the new host here and to every seam the compiler then reports"
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostOs {
    Windows,
    MacOs,
    Linux,
}

// Exactly one arm defines `CURRENT` for any given target, mirroring the predicates the backstop
// admits, so a new host fails to build here rather than silently claiming another host's lanes.
impl HostOs {
    #[cfg(windows)]
    pub const CURRENT: HostOs = HostOs::Windows;
    #[cfg(target_os = "macos")]
    pub const CURRENT: HostOs = HostOs::MacOs;
    #[cfg(target_os = "linux")]
    pub const CURRENT: HostOs = HostOs::Linux;
}

#[cfg(test)]
mod tests {
    use super::HostOs;

    #[test]
    fn the_running_host_is_one_of_the_declared_three() {
        // The value is a compile-time routing fact; this pins that it stays consistent with the
        // portable predicates other code may still use.
        match HostOs::CURRENT {
            HostOs::Windows => assert!(cfg!(windows)),
            HostOs::MacOs => assert!(cfg!(target_os = "macos")),
            HostOs::Linux => assert!(cfg!(target_os = "linux")),
        }
    }
}
