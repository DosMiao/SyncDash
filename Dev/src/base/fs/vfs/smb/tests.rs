use super::super::error::VfsErrorKind;
use super::super::{NameRules, Support, Vfs};
use super::errors::*;
use super::*;
use crate::fs::vfs::spec::{parse, RootSpec};
use smb2::types::status::NtStatus;
use smb2::types::Command;

fn backend(s: &str) -> SmbBackend {
    let RootSpec::Endpoint(r) = parse(s) else {
        panic!()
    };
    SmbBackend::new(r).unwrap()
}

#[test]
fn share_and_sub_split() {
    let b = backend("smb://ben@server/photos/2026/07");
    assert_eq!(b.share, "photos");
    assert_eq!(b.sub, "2026/07");
    let b2 = backend("smb://server/backup");
    assert_eq!(b2.share, "backup");
    assert_eq!(b2.sub, "");
}

#[test]
fn no_share_is_refused_at_construction() {
    let RootSpec::Endpoint(r) = parse("smb://server") else {
        panic!()
    };
    assert!(SmbBackend::new(r).is_err());
}

/// The share is the tree, so it must never reappear inside the path; the phrase's
/// subdirectory must.
#[test]
fn paths_resolve_under_the_subdirectory_not_the_share() {
    let b = backend("smb://ben@server/photos/2026/07");
    assert_eq!(b.share_rel("a/b.txt"), "2026/07/a/b.txt");
    assert_eq!(b.share_rel(""), "2026/07");
    let flat = backend("smb://ben@server/photos");
    assert_eq!(flat.share_rel("a/b.txt"), "a/b.txt");
    assert_eq!(flat.share_rel(""), "");
}

/// What actually goes on the wire: backslashes, and no leading separator for the server
/// to read as an absolute path.
#[test]
fn wire_paths_are_backslashed_and_relative() {
    let b = backend("smb://ben@server/photos/2026");
    assert_eq!(b.wire_path("a/b.txt"), "2026\\a\\b.txt");
    assert_eq!(b.wire_path(""), "2026");
    let flat = backend("smb://ben@server/photos");
    assert_eq!(
        flat.wire_path(""),
        "",
        "the share root is the empty path, not '\\'"
    );
    assert_eq!(flat.wire_path("x.txt"), "x.txt");
}

#[test]
fn unconnected_backend_reports_transient_not_missing() {
    let b = backend("smb://ben@server/share");
    let e = b.stat("x").unwrap_err();
    assert_eq!(e.kind, VfsErrorKind::Transient);
}

#[test]
fn write_side_still_needs_a_connection_first() {
    let b = backend("smb://ben@server/share");
    let e = b.set_mtime("x", 0).unwrap_err();
    assert_eq!(
        e.kind,
        VfsErrorKind::Transient,
        "unconnected is a transient state, never a judgment about files"
    );
}

#[test]
fn caps_declare_the_smb_profile() {
    let c = backend("smb://ben@server/share").caps();
    assert_eq!(c.protocol, "smb");
    assert!(c.set_mtime.yes(), "the whole point of this backend");
    assert_eq!(
        c.rename_overwrite,
        Support::No,
        "ReplaceIfExists goes out as 0"
    );
    assert_eq!(c.symlink, Support::No);
    assert_eq!(c.unix_mode, Support::No);
    assert!(c.ranged_read.yes());
    assert!(!c.local_trash);
    // Nothing passes through the local path layer, so the client's rules do not apply.
    assert_eq!(c.name_rules, NameRules::Unknown);
}

/// The capability map and the run-time answers have to agree: a `No` must come back as
/// `Unsupported`, not as a silent success or a protocol error.
#[test]
fn declared_gaps_refuse_honestly() {
    let b = backend("smb://ben@server/share");
    assert_eq!(
        b.set_mode("x", 0o644).unwrap_err().kind,
        VfsErrorKind::Unsupported
    );
    assert_eq!(
        b.make_symlink("x", "y").unwrap_err().kind,
        VfsErrorKind::Unsupported
    );
    assert_eq!(
        b.read_link("x").unwrap_err().kind,
        VfsErrorKind::Unsupported
    );
}

/// The engine's delete-dir classification rides on this one code, which the crate folds
/// into a generic `Other`.
#[test]
fn directory_not_empty_is_classified_not_lumped_into_protocol() {
    let e = map_smb_err(
        "remove_dir",
        smb2::Error::Protocol {
            status: NtStatus(STATUS_DIRECTORY_NOT_EMPTY),
            command: Command::Create,
        },
    );
    assert_eq!(e.kind, VfsErrorKind::NotEmpty);
}

/// A held file and a dropped link must never read as "the file is gone" — that asymmetry
/// is the difference between an annoyance and a reverse delete.
#[test]
fn ambiguous_failures_stay_transient() {
    for status in [NtStatus::SHARING_VIOLATION, NtStatus::ACCESS_DENIED] {
        let e = map_smb_err(
            "op",
            smb2::Error::Protocol {
                status,
                command: Command::Create,
            },
        );
        assert_ne!(
            e.kind,
            VfsErrorKind::NotFound,
            "{status} must not read as absence"
        );
    }
    assert_eq!(
        map_smb_err("op", smb2::Error::Disconnected).kind,
        VfsErrorKind::Transient
    );
    assert_eq!(
        map_smb_err("op", smb2::Error::Timeout).kind,
        VfsErrorKind::Transient
    );
}

#[test]
fn a_missing_name_is_the_only_thing_that_becomes_not_found() {
    let e = map_smb_err(
        "stat",
        smb2::Error::Protocol {
            status: NtStatus::OBJECT_NAME_NOT_FOUND,
            command: Command::Create,
        },
    );
    assert_eq!(e.kind, VfsErrorKind::NotFound);
}
