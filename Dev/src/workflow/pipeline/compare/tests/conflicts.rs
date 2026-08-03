//! Opt-in conflict resolution, and the cases it must refuse.
//!
//! Delete-versus-change is never auto-resolved under any policy: one side has content the other
//! deliberately removed, and no timestamp can say which the user meant.

use super::super::*;
use super::fixtures::*;
use crate::model::plan::{Action, Side};
use crate::pipeline::compare::policy::ConflictPolicy;

#[test]
fn conflict_policy_report_is_the_default_and_changes_nothing() {
    let s = snap_named("windows", "WIN", vec![file_at("f.txt", "X", 200)]);
    let t = snap_named("macos", "MAC", vec![file_at("f.txt", "Y", 100)]);
    let plan = compare(&s, &t, "sync", None, false, &CompareOptions::default());
    assert_eq!(plan.header.conflict_count, 1);
    assert!(
        !plan.ops.iter().any(|o| o.action == Action::Move),
        "report policy must not touch anything"
    );
}

#[test]
fn conflict_copy_keeps_the_loser_and_lands_the_winner() {
    let s = snap_named(
        "windows",
        "WIN",
        vec![file_at("doc/report.pdf", "NEW", 5_000)],
    );
    let t = snap_named(
        "macos",
        "MAC",
        vec![file_at("doc/report.pdf", "OLD", 1_000)],
    );
    let opts = CompareOptions {
        conflict: ConflictPolicy::Copy,
        ..Default::default()
    };
    let plan = compare(&s, &t, "sync", None, false, &opts);

    // The loser (target, older mtime) is renamed and archived first
    let mv = plan
        .ops
        .iter()
        .find(|o| o.action == Action::Move)
        .expect("loser must be kept");
    assert_eq!(mv.side, Side::Target);
    assert_eq!(mv.from.as_deref(), Some("doc/report.pdf"));
    assert!(
        mv.path.starts_with("doc/report.sync-conflict-"),
        "{}",
        mv.path
    );
    assert!(
        mv.path.ends_with(".pdf"),
        "extension must be preserved: {}",
        mv.path
    );
    // The winner's content lands on target
    let up = plan
        .ops
        .iter()
        .find(|o| o.action == Action::Update && o.path == "doc/report.pdf")
        .unwrap();
    assert_plan_hash(up, "NEW");
    // The original conflict row is downgraded to an auditable note and no longer counts as a conflict
    assert_eq!(plan.header.conflict_count, 0);
    assert!(plan
        .ops
        .iter()
        .any(|o| o.action == Action::Note && o.reason.starts_with("auto-resolved")));
}

#[test]
fn conflict_newer_overwrites_without_a_copy() {
    let s = snap_named("windows", "WIN", vec![file_at("f.txt", "NEW", 900)]);
    let t = snap_named("macos", "MAC", vec![file_at("f.txt", "OLD", 100)]);
    let opts = CompareOptions {
        conflict: ConflictPolicy::Newer,
        ..Default::default()
    };
    let plan = compare(&s, &t, "sync", None, false, &opts);
    assert!(
        !plan.ops.iter().any(|o| o.action == Action::Move),
        "newer policy keeps no copy"
    );
    let up = plan
        .ops
        .iter()
        .find(|o| o.action == Action::Update)
        .unwrap();
    assert_eq!(up.side, Side::Target);
    assert_plan_hash(up, "NEW");
}

#[test]
fn conflict_resolution_respects_the_older_side_winning() {
    // target is newer → target wins; both the copy and the overwrite happen on the source side
    let s = snap_named("windows", "WIN", vec![file_at("f.txt", "OLD", 100)]);
    let t = snap_named("macos", "MAC", vec![file_at("f.txt", "NEW", 900)]);
    let opts = CompareOptions {
        conflict: ConflictPolicy::Copy,
        ..Default::default()
    };
    let plan = compare(&s, &t, "sync", None, false, &opts);
    let mv = plan.ops.iter().find(|o| o.action == Action::Move).unwrap();
    assert_eq!(mv.side, Side::Source);
    let up = plan
        .ops
        .iter()
        .find(|o| o.action == Action::Update && o.path == "f.txt")
        .unwrap();
    assert_eq!(up.side, Side::Source);
    assert_plan_hash(up, "NEW");
}

#[test]
fn delete_versus_change_conflicts_are_never_auto_resolved() {
    // "the other side deleted it but I changed it" — automatically arbitrating "delete or keep" is too dangerous; report only under every policy
    let s = snap_named("windows", "WIN", vec![file("f.txt", "CHANGED")]);
    let t = snap_named("macos", "MAC", Vec::new());
    let a = snap("windows", vec![file("f.txt", "ORIGINAL")]);
    let opts = CompareOptions {
        conflict: ConflictPolicy::Copy,
        ..Default::default()
    };
    let plan = compare(&s, &t, "sync", Some(&a), false, &opts);
    assert_eq!(plan.header.conflict_count, 1, "{:?}", actions(&plan));
    assert!(plan
        .ops
        .iter()
        .any(|o| o.reason.contains("deleted-on-target-but-changed-on-source")));
}

#[test]
fn conflict_names_are_well_formed() {
    let n = conflict_name("a/b/report.pdf", "WIN 01", 1_769_000_000_000);
    assert!(n.starts_with("a/b/report.sync-conflict-"), "{n}");
    assert!(
        n.ends_with("-WIN-01.pdf"),
        "host must be sanitized and extension kept: {n}"
    );
    assert!(is_conflict_copy(&n));
    // A hidden file has no extension to speak of
    let h = conflict_name(".gitignore", "H", 0);
    assert!(h.starts_with(".gitignore.sync-conflict-"), "{h}");
    assert!(!is_conflict_copy("a/b/normal.pdf"));
}

#[test]
fn conflict_copies_over_the_limit_are_pruned() {
    let mut entries = vec![file("f.txt", "SAME")];
    for i in 1..=4 {
        entries.push(file(
            &format!("f.sync-conflict-2026070{i}-120000-MAC.txt"),
            &format!("c{i}"),
        ));
    }
    let s = snap_named("windows", "WIN", vec![file("f.txt", "SAME")]);
    let t = snap_named("macos", "MAC", entries);
    let opts = CompareOptions {
        conflict: ConflictPolicy::Copy,
        max_conflicts: 2,
        ..Default::default()
    };
    let plan = compare(&s, &t, "sync", None, false, &opts);
    let pruned: Vec<&str> = plan
        .ops
        .iter()
        .filter(|o| o.reason.contains("conflict-copy-over-limit"))
        .map(|o| o.path.as_str())
        .collect();
    assert_eq!(
        pruned.len(),
        2,
        "4 copies, limit 2 -> drop the 2 oldest: {pruned:?}"
    );
    assert!(
        pruned
            .iter()
            .all(|p| p.contains("20260701") || p.contains("20260702")),
        "{pruned:?}"
    );
}
