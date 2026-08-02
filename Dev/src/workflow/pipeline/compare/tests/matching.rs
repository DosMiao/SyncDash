//! Which paths on the two sides are the same path.
//!
//! NFC/NFD and case folding decide pairing before any action is chosen, so a mistake here is a
//! spurious copy-and-delete pair rather than a mismatched flag.

use super::super::*;
use super::fixtures::*;
use crate::model::plan::Action;

#[test]
fn nfc_nfd_paths_match() {
    // "café" NFC (U+00E9) vs NFD (e + U+0301): the same file, must produce no op at all
    let nfc = "caf\u{00e9}.txt";
    let nfd = "cafe\u{0301}.txt";
    let s = snap("windows", vec![file(nfc, "h1")]);
    let t = snap("macos", vec![file(nfd, "h1")]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    assert_eq!(
        plan.ops.len(),
        0,
        "NFC/NFD spellings of the same name must match"
    );
}

#[test]
fn case_insensitive_match_and_opt_out() {
    let s = snap("windows", vec![file("Readme.md", "h1")]);
    let t = snap("macos", vec![file("readme.md", "h1")]);
    assert_eq!(
        compare(
            &s,
            &t,
            "mirror",
            None,
            false,
            &CompareOptions {
                case_insensitive: true,
                ..Default::default()
            }
        )
        .ops
        .len(),
        0
    );
    // Case-sensitive: case twins with the same hash get paired by move detection into a single rename — smarter than copy + delete
    let plan = compare(
        &s,
        &t,
        "mirror",
        None,
        false,
        &CompareOptions {
            case_insensitive: false,
            ..Default::default()
        },
    );
    assert_eq!(plan.ops.len(), 1);
    assert_eq!(plan.ops[0].action, Action::Move);
}

#[test]
fn update_keeps_target_spelling() {
    let s = snap("windows", vec![file("CAF\u{00c9}.TXT", "new")]);
    let t = snap("macos", vec![file("cafe\u{0301}.txt", "old")]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    assert_eq!(plan.ops.len(), 1);
    assert_eq!(plan.ops[0].action, Action::Update);
    assert_eq!(
        plan.ops[0].path, "cafe\u{0301}.txt",
        "update must use target's own spelling"
    );
}

#[test]
fn illegal_windows_names_become_conflicts() {
    let s = snap(
        "macos",
        vec![
            file("aux.log", "h1"),
            file("ok.txt", "h2"),
            file("bad. /x", "h3"),
        ],
    );
    let t = snap("windows", Vec::new());
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    let conflicts: Vec<_> = plan
        .ops
        .iter()
        .filter(|o| o.action == Action::Conflict)
        .collect();
    assert_eq!(
        conflicts.len(),
        2,
        "aux.log and 'bad. ' segment must be flagged"
    );
    assert!(plan
        .ops
        .iter()
        .any(|o| o.action == Action::Copy && o.path == "ok.txt"));
}

// sync-with-archive classification matrix
// State notation: E = present with content x, ∅ = absent. archive = the consensus state at the last sync.
