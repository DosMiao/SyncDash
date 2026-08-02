//! The sync-mode decision matrix, one case per row of the archive-versus-both-sides table.
//!
//! Sync is the only mode that writes in both directions, so each cell is stated explicitly rather
//! than inferred from the mirror rules.

use super::super::*;
use super::fixtures::*;
use crate::model::plan::{Action, Op, Plan, Side};
use crate::model::table::{ObservedEntry, TableArtifact};

fn plan_sync(s: Vec<ObservedEntry>, t: Vec<ObservedEntry>, a: Option<Vec<ObservedEntry>>) -> Plan {
    let s = snap("windows", s);
    let t = snap("macos", t);
    let a = a.map(|e| snap("windows", e));
    compare(
        &s,
        &t,
        "sync",
        a.as_ref(),
        false,
        &CompareOptions::default(),
    )
}
fn one(plan: &Plan) -> &Op {
    assert_eq!(
        plan.ops.len(),
        1,
        "expected exactly 1 op, got: {:?}",
        plan.ops
    );
    &plan.ops[0]
}

#[test]
fn matrix_equal_no_op() {
    let p = plan_sync(
        vec![file("a", "h")],
        vec![file("a", "h")],
        Some(vec![file("a", "h")]),
    );
    assert_eq!(p.ops.len(), 0);
}

#[test]
fn matrix_source_changed_propagates_to_target() {
    let p = plan_sync(
        vec![file("a", "h2")],
        vec![file("a", "h1")],
        Some(vec![file("a", "h1")]),
    );
    let op = one(&p);
    assert_eq!(
        (op.side.clone(), op.action.clone()),
        (Side::Target, Action::Update)
    );
}

#[test]
fn matrix_target_changed_propagates_to_source() {
    let p = plan_sync(
        vec![file("a", "h1")],
        vec![file("a", "h2")],
        Some(vec![file("a", "h1")]),
    );
    let op = one(&p);
    assert_eq!(
        (op.side.clone(), op.action.clone()),
        (Side::Source, Action::Update)
    );
}

#[test]
fn matrix_both_changed_conflict() {
    let p = plan_sync(
        vec![file("a", "h2")],
        vec![file("a", "h3")],
        Some(vec![file("a", "h1")]),
    );
    assert_eq!(one(&p).action, Action::Conflict);
    assert_eq!(p.header.conflict_count, 1);
}

#[test]
fn matrix_target_deleted_propagates_deletion() {
    // The archive has it, target doesn't, source is unchanged → delete on source
    let p = plan_sync(vec![file("a", "h1")], vec![], Some(vec![file("a", "h1")]));
    let op = one(&p);
    assert_eq!(
        (op.side.clone(), op.action.clone()),
        (Side::Source, Action::Delete)
    );
}

#[test]
fn matrix_delete_vs_edit_conflict() {
    // target deleted it but source changed it → delete-vs-edit conflict; never delete silently
    let p = plan_sync(vec![file("a", "h2")], vec![], Some(vec![file("a", "h1")]));
    assert_eq!(one(&p).action, Action::Conflict);
}

#[test]
fn matrix_new_on_source_copies() {
    let p = plan_sync(vec![file("a", "h1")], vec![], Some(vec![]));
    let op = one(&p);
    assert_eq!(
        (op.side.clone(), op.action.clone()),
        (Side::Target, Action::Copy)
    );
}

#[test]
fn matrix_move_on_source_replayed_on_target() {
    // source moved a to b; target/archive still have a → replay the move on target
    let p = plan_sync(
        vec![file("b", "h1")],
        vec![file("a", "h1")],
        Some(vec![file("a", "h1")]),
    );
    let op = one(&p);
    assert_eq!(op.action, Action::Move);
    assert_eq!(op.side, Side::Target);
    assert_eq!(op.from.as_deref(), Some("a"));
    assert_eq!(op.path, "b");
}

#[test]
fn matrix_no_archive_differ_is_conflict_and_adds_flow_both_ways() {
    let p = plan_sync(
        vec![file("a", "h1"), file("s", "hs")],
        vec![file("a", "h2"), file("t", "ht")],
        None,
    );
    assert!(p
        .ops
        .iter()
        .any(|o| o.action == Action::Conflict && o.path == "a"));
    assert!(p
        .ops
        .iter()
        .any(|o| o.action == Action::Copy && o.side == Side::Target && o.path == "s"));
    assert!(p
        .ops
        .iter()
        .any(|o| o.action == Action::Copy && o.side == Side::Source && o.path == "t"));
    assert!(
        !p.ops.iter().any(|o| o.action == Action::Delete),
        "no-archive sync must never delete"
    );
}

#[test]
fn matrix_enrich_never_deletes_or_downgrades() {
    let s = snap("windows", vec![file("only-src", "h1")]);
    let mut old = file("shared", "h-old");
    old.as_file_mut().unwrap().mtime_ms = 0;
    let mut newer_on_target = file("shared", "h-new");
    newer_on_target.as_file_mut().unwrap().mtime_ms = 999_999;
    let t = snap("macos", vec![newer_on_target, file("only-tgt", "hx")]);
    let s = TableArtifact {
        header: s.header,
        entries: vec![s.entries[0].clone(), old],
    };
    let p = compare(&s, &t, "enrich", None, false, &CompareOptions::default());
    assert!(p
        .ops
        .iter()
        .any(|o| o.action == Action::Copy && o.path == "only-src"));
    assert!(!p.ops.iter().any(|o| o.action == Action::Delete));
    assert!(
        !p.ops.iter().any(|o| o.action == Action::Update),
        "enrich must not downgrade newer target"
    );
}
