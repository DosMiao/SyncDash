//! What counts as "the same file" when the evidence is incomplete.
//!
//! An unreadable file must be a conflict, never an equality: identical size and mtime with no hash
//! on one side used to fall through to the size+mtime line and declare them the same forever,
//! because the read keeps failing.

use super::super::*;
use super::fixtures::*;
use crate::model::plan::Action;
use crate::model::table::FileIdentityObservation;

/// The exact shape that used to pass silently: identical size and mtime, no hash on one side
/// because the read failed, so `files_equal` fell through to the size+mtime line and declared
/// them the same file — forever, since the read keeps failing. A restore, `touch -r`, or an SMB
/// mtime round-trip all produce changed content under a preserved size and mtime.
#[test]
fn an_unreadable_file_is_a_conflict_not_an_equality() {
    let s = snap("linux", vec![unreadable("a.bin")]);
    let t = snap("linux", vec![file("a.bin", "deadbeef")]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    let ops: Vec<_> = plan.ops.iter().filter(|o| o.path == "a.bin").collect();
    assert_eq!(ops.len(), 1, "exactly one op for the pair: {:?}", plan.ops);
    assert_eq!(
        ops[0].action,
        Action::Conflict,
        "unreadable content must not resolve to equal or to a blind update"
    );
    assert!(
        ops[0].reason.contains("evidence-unavailable"),
        "{}",
        ops[0].reason
    );
}

/// The same pair with the read succeeding must go back to being ordinary — the guard must not
/// fire on every hashless comparison, only on a failed one.
#[test]
fn a_hashless_comparison_is_still_judged_on_size_and_mtime() {
    let bare = |path: &str| {
        let mut entry = file(path, "hashing-disabled");
        entry.as_file_mut().unwrap().identity = FileIdentityObservation::SizeAndMtime;
        entry
    };
    let s = snap("linux", vec![bare("a.bin")]);
    let t = snap("linux", vec![bare("a.bin")]);
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    assert!(
        !plan.ops.iter().any(|o| o.path == "a.bin"),
        "equal size and mtime with hashing switched off is equality, not a conflict: {:?}",
        plan.ops
    );
}
