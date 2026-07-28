//! The directories where SyncDash keeps its own state on this machine.
//!
//! Distinct from `foundation::path`, which manipulates relative-path *strings*; this module
//! answers "which directory", never "how do I splice this path".
//!
//! Pure environment-variable derivation, no crate dependencies — which is the point. These
//! paths used to live in the job module, so `settings` and `territory` both had to
//! depend on the job schema just to learn where a directory was, and `settings::config_dir`
//! recovered the config root by taking `jobs_dir().parent()`. That made the `jobs` suffix
//! load-bearing: rename it and the settings file silently relocates.
//!
//! The dependency here runs the other way now: `config_dir` is primary, everything else hangs
//! off it.

use std::path::PathBuf;

/// Config root: `%APPDATA%\syncdash` on Windows, `$HOME/.config/syncdash` elsewhere.
/// Falls back to a relative directory when neither variable is set, so a stripped
/// environment still runs instead of panicking.
pub fn config_dir() -> PathBuf {
    if let Ok(a) = std::env::var("APPDATA") {
        PathBuf::from(a).join("syncdash")
    } else if let Ok(h) = std::env::var("HOME") {
        PathBuf::from(h).join(".config").join("syncdash")
    } else {
        PathBuf::from("syncdash")
    }
}

/// One TOML per job lives here.
pub fn jobs_dir() -> PathBuf {
    config_dir().join("jobs")
}

/// Archives for sync-mode jobs.
pub fn archive_dir() -> PathBuf {
    config_dir().join("archive")
}

/// Default run-log root. `settings.log_dir` overrides it.
pub fn default_log_dir() -> PathBuf {
    config_dir().join("logs")
}

/// Machine-local state root: `%LOCALAPPDATA%\syncdash`, else `$HOME/.cache/syncdash`.
///
/// Deliberately **not** `config_dir`. That one is `%APPDATA%` (roaming), which is right for job
/// files and settings and wrong for everything below: a hash cache keyed by local path and a
/// recycle store full of file bodies must not follow a user onto another machine.
pub fn data_dir() -> PathBuf {
    if let Ok(l) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(l).join("syncdash")
    } else if let Ok(h) = std::env::var("HOME") {
        PathBuf::from(h).join(".cache").join("syncdash")
    } else {
        PathBuf::from(".syncdash")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_hangs_off_one_root() {
        let root = config_dir();
        // The suffix must not be load-bearing: each of these is derived from the root, never
        // recovered from a sibling by walking back up with parent().
        assert_eq!(jobs_dir().parent().unwrap(), root);
        assert_eq!(archive_dir().parent().unwrap(), root);
        assert_eq!(default_log_dir().parent().unwrap(), root);
    }

    #[test]
    fn leaf_names_are_distinct() {
        let names: Vec<_> = [jobs_dir(), archive_dir(), default_log_dir()]
            .iter()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }
}
