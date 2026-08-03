//! Names a destination filesystem cannot faithfully hold.
//!
//! The rules fire on the root's *recorded* naming semantics, not on this host's — an SMB share
//! served from Windows is checked as Windows even when the compare runs on macOS. A mangled name
//! is refused for deletes as well as creates, because it can address the wrong file either way.

use super::super::*;
use super::fixtures::*;
use crate::model::plan::Action;
use crate::model::table::ObservedNameRules;

#[test]
fn case_sensitive_mode_flags_a_write_that_would_clobber_a_case_twin() {
    // With case_sensitive = true, Foo.txt and foo.txt are two files,
    // but on NTFS/APFS writing the former silently overwrites the latter.
    let s = snap("windows", vec![file("Foo.txt", "A"), file("foo.txt", "B")]);
    let t = snap("windows", vec![file("foo.txt", "B")]);
    let opts = CompareOptions {
        case_insensitive: false,
        ..Default::default()
    };
    let plan = compare(&s, &t, "mirror", None, false, &opts);
    let c = plan
        .ops
        .iter()
        .find(|o| o.path == "Foo.txt")
        .expect("Foo.txt must be planned somehow");
    assert_eq!(c.action, Action::Conflict, "{:?}", c.reason);
    assert!(c.reason.contains("case-collision"), "{}", c.reason);
}

/// A VFS observation records its protocol in `header.os`, while `vfs.name_rules` records the
/// destination's actual naming semantics. Planning must use the latter or an SMB-backed Windows
/// root would bypass the name-safety gate.
#[test]
fn windows_name_check_fires_on_an_smb_root_not_just_a_local_windows_one() {
    let bad = vec![
        file("report:2024.pdf", "h1"),
        file("trail.", "h2"),
        file("notes/CON", "h3"),
        file("a?b.txt", "h4"),
    ];
    let s = snap("macos", bad.clone());
    let t = snap_vfs("smb", ObservedNameRules::Windows, vec![]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    let refused: Vec<_> = plan
        .ops
        .iter()
        .filter(|o| o.action == Action::Conflict)
        .collect();
    assert_eq!(
        refused.len(),
        4,
        "every one must be refused, got {:?}",
        actions(&plan)
    );
    assert!(
        plan.ops.iter().all(|o| o.action != Action::Copy),
        "nothing may be copied"
    );

    // The reason must classify the failure honestly — the three modes are not the same risk
    let why = |p: &str| refused.iter().find(|o| o.path == p).unwrap().reason.clone();
    assert!(
        why("report:2024.pdf").contains("alternate data stream"),
        "{}",
        why("report:2024.pdf")
    );
    assert!(
        why("trail.").contains("truncated to 'trail'"),
        "{}",
        why("trail.")
    );
    assert!(
        why("notes/CON").contains("reserved device name"),
        "{}",
        why("notes/CON")
    );
    assert!(
        why("a?b.txt").contains("refuses the character"),
        "{}",
        why("a?b.txt")
    );
}

/// Deleting a mangled name is the worst case of all, because it *succeeds* against the
/// wrong file. Measured: applying a delete of rel `trail.` removed `trail`, returned Ok,
/// and left `trail.` standing — so the next round finds it again, forever, having eaten an
/// innocent neighbour on the way. A delete must therefore be refused too, which the
/// Copy/Move-only gate did not do.
#[test]
fn a_mangled_name_is_refused_for_deletes_as_well_as_creates() {
    // mirror: target has files the source does not → deletions on the Windows target
    let s = snap("macos", vec![]);
    let t = snap(
        "windows",
        vec![
            file("trail.", "h1"),
            file("keep:me.txt", "h2"),
            file("CON", "h3"),
        ],
    );
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());

    let by_path = |p: &str| {
        plan.ops
            .iter()
            .find(|o| o.path == p)
            .map(|o| (o.action.clone(), o.reason.clone()))
    };
    let (a, why) = by_path("trail.").expect("trail. must appear");
    assert_eq!(
        a,
        Action::Conflict,
        "a delete that would hit a different file must be refused: {why}"
    );
    assert!(why.contains("truncated to 'trail'"), "{why}");
    let (a, _) = by_path("keep:me.txt").expect("the colon case must appear");
    assert_eq!(
        a,
        Action::Conflict,
        "a colon path does not address the file it names"
    );

    // A reserved device name is addressable — std deletes it cleanly, so the delete stands.
    let (a, _) = by_path("CON").expect("CON must appear");
    assert_eq!(
        a,
        Action::Delete,
        "refusing to delete a reserved name would strand it forever"
    );
}

/// The source root is the one being *read*. A mangled path there reads a different file,
/// so the copy would land the wrong bytes under the right name on a perfectly healthy target.
#[test]
fn a_mangled_name_on_the_reading_side_is_refused_too() {
    let s = snap("windows", vec![file("trail.", "h1")]);
    let t = snap("linux", vec![]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    let op = plan
        .ops
        .iter()
        .find(|o| o.path == "trail.")
        .expect("the op must exist");
    assert_eq!(op.action, Action::Conflict, "reason was: {}", op.reason);
    assert!(
        op.reason.contains("reading side"),
        "the message must say which root is at fault: {}",
        op.reason
    );
}

/// SFTP/FTP cannot tell us the server's OS. Refusing a name that is perfectly legal on
/// Linux would be wrong; saying nothing would be worse. The op proceeds, with a Note.
#[test]
fn unknown_server_rules_warn_instead_of_refusing() {
    let s = snap("macos", vec![file("report:2024.pdf", "h1")]);
    let t = snap_vfs("sftp", ObservedNameRules::Unknown, vec![]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    assert!(plan
        .ops
        .iter()
        .any(|o| o.action == Action::Copy && o.path == "report:2024.pdf"));
    let note = plan
        .ops
        .iter()
        .find(|o| o.action == Action::Note)
        .expect("a warning must exist");
    assert!(
        note.reason.contains("name-risk-on-unknown-server"),
        "{}",
        note.reason
    );
}

/// A posix target must not inherit any of this: colons and reserved names are ordinary
/// file names there, and the plan says so by staying silent.
#[test]
fn posix_targets_keep_names_windows_would_reject() {
    let s = snap(
        "macos",
        vec![file("report:2024.pdf", "h1"), file("CON", "h2")],
    );
    let t = snap("linux", vec![]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    assert_eq!(
        plan.ops.iter().filter(|o| o.action == Action::Copy).count(),
        2
    );
    assert!(plan
        .ops
        .iter()
        .all(|o| o.action != Action::Note && o.action != Action::Conflict));
}

#[test]
fn normalization_twins_reported_not_merged() {
    let s = snap(
        "linux",
        vec![
            file("caf\u{00e9}.txt", "h1"),
            file("cafe\u{0301}.txt", "h2"),
        ],
    );
    let t = snap("windows", vec![file("caf\u{00e9}.txt", "h1")]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    assert!(plan
        .ops
        .iter()
        .any(|o| o.action == Action::Note && o.reason.contains("duplicate-after-normalization")));
}
