//! What counts as "the same file" when the evidence is incomplete, and what the read-only evidence
//! layer reports about a plan that already exists.
//!
//! An unreadable file must be a conflict, never an equality: identical size and mtime with no hash
//! on one side used to fall through to the size+mtime line and declare them the same forever,
//! because the read keeps failing.

use super::super::evidence::{
    evidence, identical_page, implied_row_meta, reverse_op, row_meta, RowMeta, SideMeta,
};
use super::super::*;
use super::fixtures::*;
use crate::model::plan::{Action, Op, Side};
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

/// A reversed row is executed, not just displayed, so the reversal must not quietly drop fields.
/// `link` is the sharp one: a symlink op that loses it stops being a symlink op and takes the
/// content-copy lane instead. `mode` is inert only until Copy/Update start carrying it, at which
/// point the loss would be *selective* — the one row the user looked hardest at is the one
/// written without its mode.
///
/// This row is a symlink Update, which is also the one Update shape that stays sizeless after the
/// reversal: it publishes a link rather than content, and neither side is ever measured for it.
#[test]
fn a_reversed_symlink_op_keeps_the_fields_that_decide_how_it_is_written() {
    let source_operation = Op {
        side: Side::Target,
        action: Action::Update,
        path: "bin/node".into(),
        from: None,
        size: Some(10),
        mtime_ms: Some(5),
        hash: Some("abc".into()),
        link: Some("../nodejs/bin/node".into()),
        mode: Some(0o755),
        reason: "differs".into(),
    };
    let reversed = reverse_op(&source_operation, &RowMeta::default())
        .expect("a symlink Update reverses without any measurement of either side");
    assert_eq!(
        reversed.side,
        Side::Source,
        "the reversal is what the side is for"
    );
    assert_eq!(
        reversed.link.as_deref(),
        Some("../nodejs/bin/node"),
        "a symlink op must stay a symlink op"
    );
    assert_eq!(reversed.mode, Some(0o755), "the mode survives the reversal");
    assert_eq!(reversed.path, "bin/node");
    // The digest described the content of the side that just lost, and there is no digest of the
    // winner to put in its place, so it is dropped deliberately.
    assert_eq!(reversed.hash, None);
    assert_eq!(
        reversed.size, None,
        "a symlink op writes a link, not bytes — it stays unmeasured in either direction"
    );
}

/// The row the free-space gate has to be able to judge. A reversed Update writes onto the side that
/// was going to be written, reading the side that was going to be read from — so the bytes it will
/// write are exactly what compare measured on its new origin. Leaving `size` empty here made
/// `guard::stats` read the row as zero bytes and `check_space` skip the volume entirely.
#[test]
fn a_reversed_content_update_carries_its_new_origins_measured_size() {
    let onto_target = Op {
        side: Side::Target,
        action: Action::Update,
        path: "report.bin".into(),
        from: None,
        size: Some(100),
        mtime_ms: Some(9_000),
        hash: Some("source-digest".into()),
        link: None,
        mode: None,
        reason: "differs-master-wins".into(),
    };
    let metadata = RowMeta {
        src: Some(SideMeta {
            size: 100,
            mtime_ms: 9_000,
        }),
        dst: Some(SideMeta {
            size: 40,
            mtime_ms: 2_000,
        }),
    };

    let onto_source = reverse_op(&onto_target, &metadata).expect("an Update is reversible");
    assert_eq!(onto_source.side, Side::Source);
    assert_eq!(
        onto_source.size,
        Some(40),
        "reversed onto source, the bytes come from the target the user chose to keep"
    );
    assert_eq!(onto_source.hash, None, "no digest of the winner exists");

    let back_onto_target = reverse_op(&onto_source, &metadata).expect("an Update is reversible");
    assert_eq!(back_onto_target.side, Side::Target);
    assert_eq!(
        back_onto_target.size,
        Some(100),
        "the mirror case reads the source measurement"
    );
}

/// Fail closed: a content Update whose new origin was never measured cannot be reversed into a
/// write row, because a row with no size is a row the free-space gate silently passes.
#[test]
fn a_reversed_content_update_without_its_origin_measurement_is_refused() {
    let onto_target = Op {
        side: Side::Target,
        action: Action::Update,
        path: "report.bin".into(),
        from: None,
        size: Some(100),
        mtime_ms: Some(9_000),
        hash: None,
        link: None,
        mode: None,
        reason: "differs-master-wins".into(),
    };
    let source_side_only = RowMeta {
        src: Some(SideMeta {
            size: 100,
            mtime_ms: 9_000,
        }),
        dst: None,
    };
    assert!(reverse_op(&onto_target, &source_side_only).is_none());
    assert!(reverse_op(&onto_target, &RowMeta::default()).is_none());
}

#[test]
fn reverse_op_semantics() {
    let copy = Op {
        side: Side::Target,
        action: Action::Copy,
        path: "x".into(),
        from: None,
        size: Some(5),
        mtime_ms: Some(1),
        hash: None,
        link: None,
        mode: None,
        reason: "only-in-source".into(),
    };
    let r = reverse_op(&copy, &RowMeta::default()).unwrap();
    assert_eq!((r.side, r.action), (Side::Source, Action::Delete));

    let del = Op {
        side: Side::Target,
        action: Action::Delete,
        path: "x".into(),
        from: None,
        size: Some(5),
        mtime_ms: None,
        hash: None,
        link: None,
        mode: None,
        reason: "gone-from-source".into(),
    };
    let r = reverse_op(&del, &RowMeta::default()).unwrap();
    assert_eq!((r.side, r.action), (Side::Source, Action::Copy));

    let upd = Op {
        side: Side::Target,
        action: Action::Update,
        path: "x".into(),
        from: None,
        size: None,
        mtime_ms: None,
        hash: None,
        link: None,
        mode: None,
        reason: "differs".into(),
    };
    let measured = RowMeta {
        src: Some(SideMeta {
            size: 5,
            mtime_ms: 1,
        }),
        dst: Some(SideMeta {
            size: 9,
            mtime_ms: 2,
        }),
    };
    let r = reverse_op(&upd, &measured).unwrap();
    assert_eq!((r.side, r.action), (Side::Source, Action::Update));

    let mv = Op {
        side: Side::Target,
        action: Action::Move,
        path: "b".into(),
        from: Some("a".into()),
        size: None,
        mtime_ms: None,
        hash: None,
        link: None,
        mode: None,
        reason: "m".into(),
    };
    assert!(reverse_op(&mv, &RowMeta::default()).is_none());
}

#[test]
fn evidence_reports_both_sides_and_identical_count() {
    // same: identical on both sides; upd: on both sides but with different content; only_s: source only; only_t: target only
    let s = snap(
        "windows",
        vec![
            file_with_metadata("same.txt", "h0", 10, 1_000),
            file_with_metadata("upd.txt", "hs", 30, 9_000),
            file_with_metadata("only_s.txt", "h1", 7, 5_000),
        ],
    );
    let t = snap(
        "windows",
        vec![
            file_with_metadata("same.txt", "h0", 10, 1_000),
            file_with_metadata("upd.txt", "ht", 20, 2_000),
            file_with_metadata("only_t.txt", "h2", 4, 3_000),
        ],
    );
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    let ev = evidence(&s, &t, &plan, &CompareOptions::default());
    assert_eq!(
        ev.metas.len(),
        plan.ops.len(),
        "the evidence array must correspond one-to-one with ops"
    );
    assert_eq!(ev.identical_count, 1);
    assert_eq!(ev.identical_bytes, 10);

    let by = |name: &str| {
        let i = plan.ops.iter().position(|o| o.path == name).expect(name);
        ev.metas[i].clone()
    };
    // update row: both sides need their own measured values — today Op.size/mtime can only carry source's
    let m = by("upd.txt");
    assert_eq!(m.src.unwrap().size, 30);
    assert_eq!(m.src.unwrap().mtime_ms, 9_000);
    assert_eq!(m.dst.unwrap().size, 20);
    assert_eq!(m.dst.unwrap().mtime_ms, 2_000);
    // copy row: only the source side exists
    let m = by("only_s.txt");
    assert_eq!(m.src.unwrap().size, 7);
    assert!(m.dst.is_none());
    // delete row: only the target side exists
    let m = by("only_t.txt");
    assert!(m.src.is_none());
    assert_eq!(m.dst.unwrap().size, 4);
}

#[test]
fn identical_page_lists_only_identical_files_and_pages() {
    let make_entry = |index: usize, hash: &str| {
        file_with_metadata(
            &format!("d{}/f{index}.bin", index % 3),
            hash,
            index as u64,
            index as i64 * 1000,
        )
    };
    let source = snap(
        "windows",
        (0..10).map(|index| make_entry(index, "same")).collect(),
    );
    // The last 3 differ in content on the target side → not counted as equal
    let target = snap(
        "windows",
        (0..10)
            .map(|index| make_entry(index, if index >= 7 { "diff" } else { "same" }))
            .collect(),
    );
    let compare_options = CompareOptions::default();
    let (total, rows) = identical_page(&source, &target, &compare_options, "", 0, 100);
    assert_eq!(total, 7);
    let expected_paths = vec![
        "d0/f0.bin",
        "d0/f3.bin",
        "d0/f6.bin",
        "d1/f1.bin",
        "d1/f4.bin",
        "d2/f2.bin",
        "d2/f5.bin",
    ];
    assert_eq!(
        rows.iter().map(|row| row.path.as_str()).collect::<Vec<_>>(),
        expected_paths
    );
    let (total, second_page) = identical_page(&source, &target, &compare_options, "", 3, 3);
    assert_eq!(
        total, 7,
        "total is the post-filter total, independent of the paging window"
    );
    assert_eq!(
        second_page
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<_>>(),
        &expected_paths[3..6]
    );
    let (_, first_page) = identical_page(&source, &target, &compare_options, "", 0, 3);
    assert_eq!(
        first_page
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<_>>(),
        &expected_paths[..3]
    );
    assert!(first_page
        .iter()
        .all(|first| second_page.iter().all(|second| first.path != second.path)));
    let (total, rows) = identical_page(&source, &target, &compare_options, "D1/", 0, 100);
    assert_eq!(total as usize, rows.len());
    assert!(rows.iter().all(|row| row.path.starts_with("d1/")));
    assert!(rows
        .iter()
        .all(|row| row.source_mtime_ms == row.target_mtime_ms));
}

#[test]
fn evidence_follows_move_naming_on_each_side() {
    // Same-content rename: source is already called b.bin, target is still a.bin — each side is looked up under its own name
    let s = snap(
        "windows",
        vec![file_with_metadata("b.bin", "hm", 42, 8_000)],
    );
    let t = snap(
        "windows",
        vec![file_with_metadata("a.bin", "hm", 42, 4_000)],
    );
    let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
    let i = plan
        .ops
        .iter()
        .position(|o| o.action == Action::Move)
        .expect("move");
    let ev = evidence(&s, &t, &plan, &CompareOptions::default());
    assert_eq!(
        ev.metas[i].src.unwrap().mtime_ms,
        8_000,
        "the source side is looked up under the new name b.bin"
    );
    assert_eq!(
        ev.metas[i].dst.unwrap().mtime_ms,
        4_000,
        "the target side is looked up under the old name a.bin"
    );
}

/// The wire elides a `Copy` row's `metas` entry because the row already carries its sole side's
/// measurement. Publication, artifact validation, the CSV/reveal decoder, and the desktop table all
/// read `implied_row_meta` for that decision, so what is dropped and what is rebuilt cannot drift
/// apart — a row whose entry is elided but not reconstructible would display as unmeasured on one
/// side and sort to the bottom of every size and time column.
#[test]
fn only_a_row_that_rebuilds_its_own_evidence_may_have_it_elided() {
    let measured = |side: Side, size, mtime_ms| Op {
        side,
        action: Action::Copy,
        path: "a.bin".into(),
        from: None,
        size: Some(size),
        mtime_ms: Some(mtime_ms),
        hash: None,
        link: None,
        mode: None,
        reason: "only-in-source".into(),
    };

    // The side a copy writes *to* has nothing to measure, so exactly one side is rebuilt.
    let onto_target = measured(Side::Target, 42, 8_000);
    assert_eq!(
        implied_row_meta(&onto_target),
        Some(RowMeta {
            src: Some(SideMeta {
                size: 42,
                mtime_ms: 8_000
            }),
            dst: None,
        })
    );
    let onto_source = measured(Side::Source, 42, 8_000);
    assert_eq!(
        implied_row_meta(&onto_source).unwrap().dst.unwrap().size,
        42,
        "a copy onto the source implies the target-side measurement instead"
    );
    assert_eq!(
        row_meta(&onto_target, None),
        implied_row_meta(&onto_target).unwrap(),
        "a reader with no retained entry rebuilds exactly what publication was allowed to drop"
    );

    // A symlink copy publishes a link rather than content and is measured on neither side, so it
    // implies nothing and its entry has to survive the wire.
    let mut symlink = measured(Side::Target, 0, 0);
    symlink.size = None;
    symlink.mtime_ms = None;
    symlink.link = Some("../elsewhere".into());
    assert_eq!(implied_row_meta(&symlink), None);
    assert_eq!(row_meta(&symlink, None), RowMeta::default());

    // Only Copy rows are elidable: every other action can be present on both sides at once.
    let mut update = measured(Side::Target, 42, 8_000);
    update.action = Action::Update;
    assert_eq!(implied_row_meta(&update), None);

    // A retained entry always wins over the row's own implication.
    let retained = RowMeta {
        src: Some(SideMeta {
            size: 1,
            mtime_ms: 2,
        }),
        dst: Some(SideMeta {
            size: 3,
            mtime_ms: 4,
        }),
    };
    assert_eq!(row_meta(&onto_target, Some(&retained)), retained);
}
