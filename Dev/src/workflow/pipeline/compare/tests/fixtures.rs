//! Snapshot and entry builders shared by the compare suites.

use crate::foundation::path::RootRelativePath;
use crate::model::digest::Blake3Digest;
use crate::model::plan::{Action, Op, Plan};
use crate::model::table::{
    FileIdentityObservation, ObservedEntry, ObservedFile, ObservedMedium, ObservedNameRules,
    TableArtifact, TableEvidence, TableHeader, TableKind, VfsNote, TABLE_SCHEMA,
};

pub(super) fn digest(label: &str) -> Blake3Digest {
    Blake3Digest::hash_bytes(label.as_bytes())
}

pub(super) fn assert_plan_hash(operation: &Op, label: &str) {
    let expected = digest(label);
    assert_eq!(operation.hash.as_deref(), Some(expected.as_str()));
}

pub(super) fn snap(os: &str, entries: Vec<ObservedEntry>) -> TableArtifact {
    let evidence = if entries.iter().all(|entry| {
        entry
            .as_file()
            .is_none_or(|file| matches!(file.identity, FileIdentityObservation::SizeAndMtime))
    }) {
        TableEvidence::None
    } else {
        TableEvidence::Full
    };
    TableArtifact {
        header: TableHeader {
            schema: TABLE_SCHEMA,
            kind: TableKind::Snapshot,
            root: "/r".into(),
            host: "h".into(),
            os: os.into(),
            scanned_at_ms: 0,
            duration_ms: 0,
            entry_count: entries.len() as u64,
            evidence,
            excluded_dirs: 0,
            excluded_files: 0,
            walk_errors: 0,
            walk_err_samples: Vec::new(),
            icloud_stubs: 0,
            icloud_stub_samples: Vec::new(),
            dataless_files: 0,
            skipped_symlinks: 0,
            vfs: None,
        },
        entries,
    }
}
pub(super) fn file(path: &str, hash: &str) -> ObservedEntry {
    ObservedEntry::File(ObservedFile {
        path: RootRelativePath::try_from(path).unwrap(),
        size: 1,
        mtime_ms: 0,
        identity: FileIdentityObservation::FullBlake3 {
            digest: digest(hash),
        },
        file_system_id: None,
        mode: None,
        previous_identities: Vec::new(),
    })
}
/// A file whose content could not be read: same size and mtime as its twin, no hash.
pub(super) fn unreadable(path: &str) -> ObservedEntry {
    let mut entry = file(path, "unreadable");
    entry.as_file_mut().unwrap().identity = FileIdentityObservation::Unreadable;
    entry
}

/// A file with an mtime (conflict arbitration goes by mtime)
pub(super) fn file_at(path: &str, hash: &str, mtime_ms: i64) -> ObservedEntry {
    let mut entry = file(path, hash);
    entry.as_file_mut().unwrap().mtime_ms = mtime_ms;
    entry
}
pub(super) fn sized(path: &str, hash: &str, size: u64) -> ObservedEntry {
    let mut entry = file(path, hash);
    entry.as_file_mut().unwrap().size = size;
    entry
}
/// An archive entry: current hash + historic generations
pub(super) fn arch(path: &str, hash: &str, previous: &[&str]) -> ObservedEntry {
    let mut entry = file(path, hash);
    entry.as_file_mut().unwrap().previous_identities = previous
        .iter()
        .map(|label| FileIdentityObservation::FullBlake3 {
            digest: digest(label),
        })
        .collect();
    entry
}
pub(super) fn snap_named(os: &str, host: &str, entries: Vec<ObservedEntry>) -> TableArtifact {
    let mut s = snap(os, entries);
    s.header.host = host.into();
    s
}
/// A snapshot of a VFS root: `header.os` carries the *protocol*, and the naming rules
/// live in the VfsNote — exactly the shape `scan_vfs` writes.
pub(super) fn snap_vfs(
    protocol: &str,
    name_rules: ObservedNameRules,
    entries: Vec<ObservedEntry>,
) -> TableArtifact {
    let mut s = snap(protocol, entries);
    s.header.vfs = Some(VfsNote {
        protocol: protocol.into(),
        display_root: "/r".into(),
        mtime_precision_ms: 1,
        medium: ObservedMedium::NetworkShare,
        name_rules,
        degraded: Vec::new(),
    });
    s
}
pub(super) fn actions(plan: &Plan) -> Vec<(&str, &str)> {
    plan.ops
        .iter()
        .map(|o| {
            (
                match o.action {
                    Action::Copy => "copy",
                    Action::Update => "update",
                    Action::Move => "move",
                    Action::Delete => "delete",
                    Action::DeleteDir => "deletedir",
                    Action::Chmod => "chmod",
                    Action::Conflict => "conflict",
                    Action::Note => "note",
                },
                o.path.as_str(),
            )
        })
        .collect()
}

// Empty files and ambiguous move pairing.
