//! Symlink planning across the three modes.

use super::super::*;
use super::fixtures::*;
use crate::foundation::path::RootRelativePath;
use crate::model::plan::{Action, Op, Side};
use crate::model::table::ObservedEntry;
use crate::pipeline::compare::policy::ConflictPolicy;

fn link(path: &str, target: &str) -> ObservedEntry {
    ObservedEntry::Symlink(crate::model::table::ObservedSymlink {
        path: RootRelativePath::try_from(path).unwrap(),
        mtime_ms: 0,
        target: target.into(),
    })
}

fn link_ops(mode: &str, source: Vec<ObservedEntry>, target: Vec<ObservedEntry>) -> Vec<Op> {
    compare(
        &snap("linux", source),
        &snap("linux", target),
        mode,
        None,
        false,
        &CompareOptions::default(),
    )
    .ops
}

/// Symlink planning decides Copy/Update/Delete/Conflict across three modes and had no test.
/// These are the four decisions that can move or destroy a link.
#[test]
fn symlink_planning_differs_by_mode() {
    // Only in source: every mode propagates it to the target.
    for mode in ["mirror", "sync", "enrich"] {
        let ops = link_ops(mode, vec![link("l", "/a")], Vec::new());
        assert_eq!(ops.len(), 1, "{mode}");
        assert_eq!(ops[0].action, Action::Copy, "{mode}");
        assert_eq!(ops[0].side, Side::Target, "{mode}");
        assert_eq!(ops[0].link.as_deref(), Some("/a"), "{mode}");
    }

    // Only in target: mirror deletes it, sync adopts it back, enrich leaves it alone.
    let ops = link_ops("mirror", Vec::new(), vec![link("l", "/b")]);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].action, Action::Delete);
    assert_eq!(ops[0].side, Side::Target);

    let ops = link_ops("sync", Vec::new(), vec![link("l", "/b")]);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].action, Action::Copy);
    assert_eq!(
        ops[0].side,
        Side::Source,
        "sync adopts a target-only link rather than deleting it"
    );

    assert!(link_ops("enrich", Vec::new(), vec![link("l", "/b")]).is_empty());
}

/// A link whose target text differs is a content disagreement with no bytes to arbitrate.
/// mirror lets the master win; sync reports it and must never auto-resolve, because resolution
/// retains the loser as a copy of its content and a link has none.
#[test]
fn a_differing_symlink_is_reported_in_sync_and_never_auto_resolved() {
    let ops = link_ops("mirror", vec![link("l", "/a")], vec![link("l", "/b")]);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].action, Action::Update);
    assert_eq!(ops[0].link.as_deref(), Some("/a"));

    let ops = link_ops("sync", vec![link("l", "/a")], vec![link("l", "/b")]);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].action, Action::Conflict);
    assert_eq!(ops[0].reason, "symlink-differs");

    // The same case under either opt-in resolution policy stays a Conflict: resolution arbitrates
    // file bytes, and a link has none.
    for policy in [ConflictPolicy::Copy, ConflictPolicy::Newer] {
        let resolving = CompareOptions {
            conflict: policy,
            ..CompareOptions::default()
        };
        let ops = compare(
            &snap("linux", vec![link("l", "/a")]),
            &snap("linux", vec![link("l", "/b")]),
            "sync",
            None,
            false,
            &resolving,
        )
        .ops;
        assert_eq!(ops.len(), 1, "{policy:?}");
        assert_eq!(
            ops[0].action,
            Action::Conflict,
            "a symlink conflict has no content to arbitrate, so it stays reported under {policy:?}"
        );
    }

    // Identical targets are not a change at all.
    assert!(link_ops("sync", vec![link("l", "/a")], vec![link("l", "/a")]).is_empty());
}
