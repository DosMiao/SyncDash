//! What this machine reports about itself, and how to spell it inside a filename.
//!
//! These three facts are stamped into snapshots, plans, lock ledgers, retained versions, root
//! markers and transfer packages — six persisted formats owned by four different layers. They lived
//! in the snapshot format module, so a lock file and a package each reached into `model::table` to
//! learn the hostname, which reads as if the table format owned machine identity.
//!
//! `safe_host` belongs with them rather than with general text handling: it exists because a
//! hostname is written into conflict-copy filenames, and a host containing `/`, `:` or a space
//! would otherwise produce a name no filesystem accepts — the rule is about identity reaching a
//! filename, not about text.

/// The operating system this build is running on, as recorded in snapshot headers.
pub fn os_name() -> String {
    std::env::consts::OS.to_string()
}

/// This machine's hostname, or `"unknown"` when it cannot be determined.
///
/// A failure degrades rather than propagates: every caller is stamping provenance into an artifact
/// it is about to write, and refusing to write a snapshot because the hostname is unreadable would
/// trade a cosmetic gap for a lost run.
pub fn host_name() -> String {
    hostname::get()
        .map(|host| host.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into())
}

/// Hostname → a form safe inside a filename (anything that is not alphanumeric or `-` becomes `-`).
pub fn safe_host(host: &str) -> String {
    host.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hostname_becomes_filename_safe_without_becoming_empty() {
        assert_eq!(safe_host("build-01"), "build-01");
        assert_eq!(safe_host("mac.local"), "mac-local");
        assert_eq!(safe_host("dept/nas:2"), "dept-nas-2");
        assert_eq!(
            safe_host("WIN01"),
            "WIN01",
            "an already-safe host maps to itself"
        );
        assert_eq!(safe_host("my.host:2222"), "my-host-2222");
        // Fixture data: the assertion is that non-ASCII host characters each collapse to '-'.
        assert_eq!(safe_host("主机"), "--");
    }

    #[test]
    fn machine_identity_always_answers() {
        assert!(
            !host_name().is_empty(),
            "an unreadable hostname degrades to a value, never a failure"
        );
        assert!(!os_name().is_empty());
    }
}
