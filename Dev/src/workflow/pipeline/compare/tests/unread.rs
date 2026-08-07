//! Subtrees a scan was refused, and the deletions they must not become.
//!
//! The whole reason the scan records an unread path instead of aborting is that these assertions
//! can be made: the table names the subtree, and compare is blind inside it on both sides. Without
//! the suppression the scan would be handing compare a directory with zero children, which under
//! mirror is a delete for every file the other side still holds.

use super::super::*;
use super::fixtures::*;

/// The data-loss case, stated directly. The source could not read `locked/`, the target has files
/// there, and mirror must not propose removing them.
#[test]
fn a_subtree_the_source_could_not_read_produces_no_deletions_on_the_target() {
    let source = snap_unread(
        "windows",
        vec![directory("locked"), file("keep.txt", "A")],
        &["locked"],
    );
    let target = snap(
        "windows",
        vec![
            directory("locked"),
            file("locked/secret.txt", "B"),
            file("locked/deep/other.txt", "C"),
            file("keep.txt", "A"),
        ],
    );

    let plan = compare(
        &source,
        &target,
        "mirror",
        None,
        false,
        &CompareOptions::default(),
    );
    assert!(
        actions(&plan).is_empty(),
        "nothing under an unread subtree may be planned: {:?}",
        actions(&plan)
    );
}

/// The other direction of the same blindness: content the *target* could not read must not be
/// copied over from the source either, because there is no evidence it is missing.
#[test]
fn suppression_applies_to_the_side_that_could_read_the_subtree() {
    let source = snap(
        "windows",
        vec![directory("locked"), file("locked/secret.txt", "B")],
    );
    let target = snap_unread("windows", vec![directory("locked")], &["locked"]);

    let plan = compare(
        &source,
        &target,
        "mirror",
        None,
        false,
        &CompareOptions::default(),
    );
    assert!(
        actions(&plan).is_empty(),
        "a subtree only the target could not read is still unjudgeable: {:?}",
        actions(&plan)
    );
}

/// Suppression is scoped, not a blanket amnesty — the rest of the tree still compares.
#[test]
fn differences_outside_the_unread_subtree_are_still_planned() {
    let source = snap_unread(
        "windows",
        vec![directory("locked"), file("new.txt", "A")],
        &["locked"],
    );
    let target = snap(
        "windows",
        vec![directory("locked"), file("locked/x.txt", "B")],
    );

    let plan = compare(
        &source,
        &target,
        "mirror",
        None,
        false,
        &CompareOptions::default(),
    );
    assert_eq!(
        actions(&plan),
        vec![("copy", "new.txt")],
        "only the unread subtree is out of scope"
    );
}

/// Whole-segment prefixes only. A sibling whose name merely starts with the unread path is a
/// different directory, and silently skipping it would be the same class of bug in reverse.
#[test]
fn a_name_that_merely_starts_with_an_unread_path_is_not_suppressed() {
    let source = snap_unread("windows", vec![directory("locked")], &["locked"]);
    let target = snap(
        "windows",
        vec![directory("locked"), file("locked-old/x.txt", "B")],
    );

    let plan = compare(
        &source,
        &target,
        "mirror",
        None,
        false,
        &CompareOptions::default(),
    );
    assert_eq!(
        actions(&plan),
        vec![("delete", "locked-old/x.txt")],
        "'locked-old' is not inside 'locked'"
    );
}

/// Deleting a directory deletes everything under it, so an ancestor deletion is the one way the
/// per-path suppression can be defeated from one level up. Found by running the real case: with
/// `.claude` absent from the source, mirror planned `delete_dir .claude` — which would have taken
/// the protected subtree with it.
#[test]
fn a_directory_enclosing_an_unread_subtree_is_never_deleted() {
    let source = snap_unread("windows", vec![], &["keep/locked"]);
    let target = snap(
        "windows",
        vec![
            directory("keep"),
            directory("keep/locked"),
            file("keep/locked/secret.txt", "B"),
            directory("gone"),
        ],
    );

    let plan = compare(
        &source,
        &target,
        "mirror",
        None,
        false,
        &CompareOptions::default(),
    );
    assert_eq!(
        actions(&plan),
        vec![("deletedir", "gone")],
        "'keep' encloses an unread subtree and must survive; 'gone' encloses nothing"
    );
}

/// The plan has to carry the paths, because preflight and the webview both run long after the
/// snapshots are gone.
#[test]
fn the_plan_header_names_the_unread_subtrees_and_counts_what_they_hid() {
    let source = snap_unread("windows", vec![directory("locked")], &["locked"]);
    let target = snap(
        "windows",
        vec![directory("locked"), file("locked/secret.txt", "B")],
    );

    let plan = compare(
        &source,
        &target,
        "mirror",
        None,
        false,
        &CompareOptions::default(),
    );
    assert_eq!(plan.header.source_unread_paths, vec!["locked".to_string()]);
    assert!(plan.header.target_unread_paths.is_empty());
    // Counted against the union, so the side that *could* read the subtree reports what it holds
    // there — those are exactly the entries that would otherwise have been planned away.
    assert_eq!(plan.header.source_unread_entries, 1);
    assert_eq!(plan.header.target_unread_entries, 2);
}
