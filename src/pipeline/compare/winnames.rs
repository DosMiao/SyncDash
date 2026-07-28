//! Windows naming legality, decided at plan time.
//!
//! A name legal on APFS can be unwritable on NTFS — reserved device names, `< > : " | ? *`, a
//! trailing dot or space. Catching it here turns "the sync failed two hours in" into a Conflict
//! row visible before anything is written.
//!
//! Which rules apply is a property of the path layer the write travels through, not of the far
//! machine: a Windows client writing to a Linux Samba box still gets Win32 parsing.

use crate::fs::vfs::NameRules;
use crate::model::table::Header;

/// The naming rules writes to this snapshot's root are subject to.
///
/// A VFS root's `Header.os` is the *protocol* ("smb", "sftp", "ftp"), so the old
/// `header.os == "windows"` gate silently skipped every remote root — including `smb://`,
/// which is precisely where Win32 name parsing does apply, because the local client performs
/// it before anything reaches the wire. Plain local roots have no `VfsNote` and keep answering
/// from `os`, so their behavior is unchanged.
pub(super) fn name_rules_of(h: &Header) -> NameRules {
    match &h.vfs {
        Some(v) if !v.name_rules.is_empty() => NameRules::parse(&v.name_rules),
        // Snapshots written before this field existed, and plain local roots.
        _ => {
            if h.os == "windows" {
                NameRules::Windows
            } else if h.vfs.is_some() {
                NameRules::Unknown
            } else {
                NameRules::Posix
            }
        }
    }
}

/// How badly a Windows-semantics root handles this path. Ordered by blast radius.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum WinNameFault {
    /// The path does not address the file it names. **Every** operation touching this root
    /// is unsafe, deletes and reads included — not just creation.
    Mangled,
    /// Creating it fails outright. Reading or deleting is moot: such a file cannot be there.
    Rejected,
    /// Creating, reading and deleting all work; other Windows software cannot cope.
    Unusable,
}

/// Why this relative path cannot be handled faithfully on a Windows-semantics root.
///
/// The three faults differ in blast radius and the reason text says which — a plan that claims
/// "illegal" for a name Windows would in fact accept is the same kind of lie as a silent
/// exclusion. All three were measured on Win11 26200 through the same std calls the engine uses:
///
/// - **Rejected**: `< > " | ? *` and control chars — the write really does fail (os error 123).
/// - **Mangled**: the dangerous class, because the operation *succeeds* against the wrong file.
///   `report:2024.pdf` writes into an NTFS alternate data stream: `write` returns `Ok`, and
///   `read_dir` then lists only `report`. A trailing dot or space is stripped by path
///   normalization, so `trail.` and `trail ` both resolve to `trail`. This is not only a
///   creation hazard: scanning walks from a `\\?\`-prefixed root, which suppresses that
///   normalization, so the scan *can* see a literal `trail.` that the apply lane then cannot
///   address. Measured end to end — applying a delete of rel `trail.` removed `trail`,
///   returned `Ok`, and left `trail.` sitting there for the next round to find again.
/// - **Unusable**: reserved device names. These are *not* rejected — `CON`, `com1`, `nul.txt.jpg`
///   all wrote, read, listed and deleted cleanly, because std addresses files through `\\?\`
///   verbatim paths (and Windows 11 relaxed the rule besides). We still refuse to create them,
///   as FreeFileSync and syncthing do, because Explorer, cmd and most Win32 software cannot
///   open or delete such a file afterwards — but the reason must not claim Windows forbade it,
///   and a *delete* of one is perfectly safe to carry out.
pub(super) fn win_name_fault(rel: &str) -> Option<(WinNameFault, String)> {
    win_invalid_reason(rel).map(|r| {
        let fault = if r.starts_with("mangled") {
            WinNameFault::Mangled
        } else if r.starts_with("rejected") {
            WinNameFault::Rejected
        } else {
            WinNameFault::Unusable
        };
        (fault, r)
    })
}

pub(super) fn win_invalid_reason(rel: &str) -> Option<String> {
    // COM0/LPT0 are absent from the Microsoft list but Explorer treats them as reserved too
    // (syncthing carries the same two extras, with the same note).
    const RESERVED: [&str; 24] = [
        "CON", "PRN", "AUX", "NUL",
        "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    for seg in rel.split('/') {
        if seg.is_empty() {
            continue;
        }
        if let Some(stripped) = seg.strip_suffix('.').or_else(|| seg.strip_suffix(' ')) {
            return Some(format!(
                "mangled: '{seg}' would be silently truncated to '{stripped}' and could overwrite a different file"
            ));
        }
        if seg.contains(':') {
            return Some(format!(
                "mangled: '{seg}' would be written into an alternate data stream — the write reports success, but the name disappears from the directory"
            ));
        }
        if let Some(c) = seg.chars().find(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*' | '\\') || (*c as u32) < 0x20) {
            return Some(format!("rejected: Windows refuses the character {c:?} in '{seg}'"));
        }
        let base = seg.split('.').next().unwrap_or("").to_ascii_uppercase();
        if RESERVED.contains(&base.as_str()) {
            return Some(format!(
                "unusable: '{seg}' is a reserved device name — it can be created, but Explorer and most Windows programs cannot open or delete it afterwards"
            ));
        }
    }
    None
}
