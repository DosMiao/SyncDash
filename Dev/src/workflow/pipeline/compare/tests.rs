use super::*;
use crate::foundation::path::RootRelativePath;
use crate::model::plan::{Action, Op, Plan, Side};
use crate::model::table::{
    Blake3Digest, FileIdentityObservation, ObservedEntry, ObservedFile, ObservedMedium,
    ObservedNameRules, TableArtifact, TableEvidence, TableHeader, TableKind, VfsNote, TABLE_SCHEMA,
};

fn digest(label: &str) -> Blake3Digest {
    Blake3Digest::hash_bytes(label.as_bytes())
}

fn assert_plan_hash(operation: &Op, label: &str) {
    let expected = digest(label);
    assert_eq!(operation.hash.as_deref(), Some(expected.as_str()));
}

fn snap(os: &str, entries: Vec<ObservedEntry>) -> TableArtifact {
    let evidence = if entries.iter().all(|entry| {
        entry
            .as_file()
            .is_none_or(|file| matches!(file.identity, FileIdentityObservation::SizeAndMtime))
    }) {
        TableEvidence::None
    } else {
        TableEvidence::Full
    };
    TableArtifact {
        header: TableHeader {
            schema: TABLE_SCHEMA,
            kind: TableKind::Snapshot,
            root: "/r".into(),
            host: "h".into(),
            os: os.into(),
            scanned_at_ms: 0,
            duration_ms: 0,
            entry_count: entries.len() as u64,
            evidence,
            excluded_dirs: 0,
            excluded_files: 0,
            walk_errors: 0,
            walk_err_samples: Vec::new(),
            icloud_stubs: 0,
            icloud_stub_samples: Vec::new(),
            dataless_files: 0,
            skipped_symlinks: 0,
            vfs: None,
        },
        entries,
    }
}
fn file(path: &str, hash: &str) -> ObservedEntry {
    ObservedEntry::File(ObservedFile {
        path: RootRelativePath::try_from(path).unwrap(),
        size: 1,
        mtime_ms: 0,
        identity: FileIdentityObservation::FullBlake3 {
            digest: digest(hash),
        },
        file_system_id: None,
        mode: None,
        previous_identities: Vec::new(),
    })
}
/// A file whose content could not be read: same size and mtime as its twin, no hash.
fn unreadable(path: &str) -> ObservedEntry {
    let mut entry = file(path, "unreadable");
    entry.as_file_mut().unwrap().identity = FileIdentityObservation::Unreadable;
    entry
}

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
/// A file with an mtime (conflict arbitration goes by mtime)
fn file_at(path: &str, hash: &str, mtime_ms: i64) -> ObservedEntry {
    let mut entry = file(path, hash);
    entry.as_file_mut().unwrap().mtime_ms = mtime_ms;
    entry
}
fn sized(path: &str, hash: &str, size: u64) -> ObservedEntry {
    let mut entry = file(path, hash);
    entry.as_file_mut().unwrap().size = size;
    entry
}
/// An archive entry: current hash + historic generations
fn arch(path: &str, hash: &str, previous: &[&str]) -> ObservedEntry {
    let mut entry = file(path, hash);
    entry.as_file_mut().unwrap().previous_identities = previous
        .iter()
        .map(|label| FileIdentityObservation::FullBlake3 {
            digest: digest(label),
        })
        .collect();
    entry
}
fn snap_named(os: &str, host: &str, entries: Vec<ObservedEntry>) -> TableArtifact {
    let mut s = snap(os, entries);
    s.header.host = host.into();
    s
}
/// A snapshot of a VFS root: `header.os` carries the *protocol*, and the naming rules
/// live in the VfsNote — exactly the shape `scan_vfs` writes.
fn snap_vfs(
    protocol: &str,
    name_rules: ObservedNameRules,
    entries: Vec<ObservedEntry>,
) -> TableArtifact {
    let mut s = snap(protocol, entries);
    s.header.vfs = Some(VfsNote {
        protocol: protocol.into(),
        display_root: "/r".into(),
        mtime_precision_ms: 1,
        medium: ObservedMedium::NetworkShare,
        name_rules,
        degraded: Vec::new(),
    });
    s
}
fn actions(plan: &Plan) -> Vec<(&str, &str)> {
    plan.ops
        .iter()
        .map(|o| {
            (
                match o.action {
                    Action::Copy => "copy",
                    Action::Update => "update",
                    Action::Move => "move",
                    Action::Delete => "delete",
                    Action::DeleteDir => "deletedir",
                    Action::Chmod => "chmod",
                    Action::Conflict => "conflict",
                    Action::Note => "note",
                },
                o.path.as_str(),
            )
        })
        .collect()
}

// Empty files and ambiguous move pairing.

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

// Unix permission bits.

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
