//! Pairing a delete with a create as one move, and the generation history that arbitrates.
//!
//! A wrongly-paired move deletes one file and leaves another in its place, so ambiguity is
//! labelled rather than guessed and empty files are never paired at all.

use super::super::*;
use super::fixtures::*;
use crate::model::plan::{Action, Side};
use crate::model::table::TableEvidence;

#[test]
fn empty_files_are_never_paired_as_moves() {
    // Every zero-length file has the same blake3. They used to get paired into a pile of "renames" —
    // the resulting content was right, but the attribution was invented. syncthing simply excludes Size == 0 in findRename.
    let e = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    let s = snap(
        "windows",
        vec![sized("new/a.py", e, 0), sized("new/b.py", e, 0)],
    );
    let t = snap(
        "windows",
        vec![sized("old/x.py", e, 0), sized("old/y.py", e, 0)],
    );
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    assert!(
        !plan.ops.iter().any(|o| o.action == Action::Move),
        "zero-length files must never be paired as renames: {:?}",
        actions(&plan)
    );
    assert_eq!(
        plan.ops.iter().filter(|o| o.action == Action::Copy).count(),
        2
    );
    assert_eq!(
        plan.ops
            .iter()
            .filter(|o| o.action == Action::Delete)
            .count(),
        2
    );
}

#[test]
fn ambiguous_move_is_labeled_as_such() {
    // Several candidates with the same content: the pairing's content is correct, but from is picked arbitrarily — reason must tell the truth
    let s = snap("windows", vec![sized("moved/one.bin", "h", 10)]);
    let t = snap(
        "windows",
        vec![
            sized("a/one.bin", "h", 10),
            sized("b/one.bin", "h", 10),
            sized("c/one.bin", "h", 10),
        ],
    );
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    let mv = plan
        .ops
        .iter()
        .find(|o| o.action == Action::Move)
        .expect("should still pair one");
    assert!(
        mv.reason.contains("ambiguous"),
        "reason must admit the ambiguity, got {:?}",
        mv.reason
    );
    assert!(
        mv.reason.contains('3'),
        "and say how many candidates: {:?}",
        mv.reason
    );
}

#[test]
fn unambiguous_move_stays_clean() {
    let s = snap("windows", vec![sized("moved/one.bin", "h", 10)]);
    let t = snap("windows", vec![sized("one.bin", "h", 10)]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    let mv = plan.ops.iter().find(|o| o.action == Action::Move).unwrap();
    assert!(
        !mv.reason.contains("ambiguous"),
        "a single candidate must not be flagged: {:?}",
        mv.reason
    );
}

#[test]
fn full_file_identities_enable_moves_inside_a_sampled_table() {
    let mut source = snap("windows", vec![sized("moved/one.bin", "h", 10)]);
    let mut target = snap("windows", vec![sized("one.bin", "h", 10)]);
    source.header.evidence = TableEvidence::Sampled;
    target.header.evidence = TableEvidence::Sampled;

    let plan = compare(
        &source,
        &target,
        "mirror",
        None,
        false,
        &CompareOptions::default(),
    );

    assert_eq!(
            plan.ops
                .iter()
                .filter(|operation| operation.action == Action::Move)
                .count(),
            1,
            "the candidate files carry full evidence even though larger files in the table may be sampled"
        );
}

// Multi-generation archive attribution.

#[test]
fn a_side_that_is_merely_behind_is_not_a_conflict() {
    // The archive has advanced to H2; source moved on to H3, target is still stuck at H1 (last sync didn't complete).
    // Previously both sides != the archive's current generation → false both-changed.
    let s = snap("windows", vec![file("f.txt", "H3")]);
    let t = snap("macos", vec![file("f.txt", "H1")]);
    let a = snap("windows", vec![arch("f.txt", "H2", &["H1", "H0"])]);
    let plan = compare(&s, &t, "sync", Some(&a), false, &CompareOptions::default());
    assert_eq!(
        plan.header.conflict_count,
        0,
        "being behind is not concurrent editing: {:?}",
        actions(&plan)
    );
    let up = plan
        .ops
        .iter()
        .find(|o| o.action == Action::Update)
        .expect("should propagate");
    assert_eq!(up.side, Side::Target);
    assert_plan_hash(up, "H3");
}

#[test]
fn genuinely_novel_content_on_both_sides_is_still_a_conflict() {
    // Neither side's content was ever seen by the archive → this is the genuine concurrent edit; the multi-generation logic must never let it slip
    let s = snap("windows", vec![file("f.txt", "X")]);
    let t = snap("macos", vec![file("f.txt", "Y")]);
    let a = snap("windows", vec![arch("f.txt", "H2", &["H1", "H0"])]);
    let plan = compare(&s, &t, "sync", Some(&a), false, &CompareOptions::default());
    assert_eq!(plan.header.conflict_count, 1, "{:?}", actions(&plan));
}

#[test]
fn newer_generation_wins_when_both_sides_are_behind() {
    // source sits at generation 1, target at generation 2 → source is newer, propagate to target
    let s = snap("windows", vec![file("f.txt", "H1")]);
    let t = snap("macos", vec![file("f.txt", "H0")]);
    let a = snap("windows", vec![arch("f.txt", "H2", &["H1", "H0"])]);
    let plan = compare(&s, &t, "sync", Some(&a), false, &CompareOptions::default());
    assert_eq!(plan.header.conflict_count, 0);
    let up = plan
        .ops
        .iter()
        .find(|o| o.action == Action::Update)
        .unwrap();
    assert_eq!(up.side, Side::Target);
    assert!(up.reason.contains("behind-by-generations"), "{}", up.reason);
}

#[test]
fn roll_generations_builds_the_history_chain() {
    use crate::model::table::roll_generations;
    let old = vec![arch("f.txt", "H1", &["H0"])];
    let mut fresh = vec![file("f.txt", "H2")];
    roll_generations(&mut fresh, &old);
    let fresh_history = &fresh[0].as_file().unwrap().previous_identities;
    assert_eq!(fresh_history.len(), 2);
    assert_eq!(fresh_history[0].digest(), Some(&digest("H1")));
    assert_eq!(fresh_history[1].digest(), Some(&digest("H0")));

    // When the content hasn't changed, the same hash must not be poured into the history
    let mut same = vec![file("f.txt", "H1")];
    roll_generations(&mut same, &old);
    let same_history = &same[0].as_file().unwrap().previous_identities;
    assert_eq!(same_history.len(), 1);
    assert_eq!(same_history[0].digest(), Some(&digest("H0")));
}

// Conflict copies.
