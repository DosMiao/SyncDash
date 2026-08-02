//! Permission bits: when a mode difference is work, and when it is noise.

use super::super::*;
use super::fixtures::*;

#[test]
fn mode_only_difference_produces_a_chmod_not_a_recopy() {
    let mut se = file("run.sh", "SAME");
    se.as_file_mut().unwrap().mode = Some(0o755);
    let mut te = file("run.sh", "SAME");
    te.as_file_mut().unwrap().mode = Some(0o644);
    let s = snap("macos", vec![se]);
    let t = snap("linux", vec![te]);
    let opts = CompareOptions {
        sync_mode: true,
        ..Default::default()
    };
    let plan = compare(&s, &t, "mirror", None, false, &opts);
    assert_eq!(
        actions(&plan),
        vec![("chmod", "run.sh")],
        "content is identical; only the bits differ"
    );
    assert_eq!(plan.ops[0].mode, Some(0o755));
}

#[test]
fn mode_is_ignored_unless_enabled_and_both_sides_are_unix() {
    let mut se = file("run.sh", "SAME");
    se.as_file_mut().unwrap().mode = Some(0o755);
    let mut te = file("run.sh", "SAME");
    te.as_file_mut().unwrap().mode = Some(0o644);
    // Off by default
    let plan = compare(
        &snap("macos", vec![se.clone()]),
        &snap("linux", vec![te.clone()]),
        "mirror",
        None,
        false,
        &CompareOptions::default(),
    );
    assert!(plan.ops.is_empty());
    // The Windows side has no mode, so even switched on it must not report a difference
    let opts = CompareOptions {
        sync_mode: true,
        ..Default::default()
    };
    let plan2 = compare(
        &snap("macos", vec![se]),
        &snap("windows", vec![te]),
        "mirror",
        None,
        false,
        &opts,
    );
    assert!(plan2.ops.is_empty(), "{:?}", actions(&plan2));
}

#[test]
fn copies_carry_the_source_mode_when_enabled() {
    let mut se = file("new.sh", "H");
    se.as_file_mut().unwrap().mode = Some(0o755);
    let s = snap("macos", vec![se]);
    let t = snap("linux", Vec::new());
    let opts = CompareOptions {
        sync_mode: true,
        ..Default::default()
    };
    let plan = compare(&s, &t, "mirror", None, false, &opts);
    assert_eq!(plan.ops.len(), 1);
    assert_eq!(
        plan.ops[0].mode,
        Some(0o755),
        "a fresh copy must land with the right bits in one step"
    );
}

// Case collisions.
