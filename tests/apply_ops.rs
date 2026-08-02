//! `pipeline::apply` against real directories.
//!
//! These were inline `#[cfg(test)]` tests, but every one of them builds a temp tree and drives
//! the public `apply` entry point — they are integration tests that happened to live inside the
//! module. Out here they keep `apply/mod.rs` to its actual production size, and they exercise the
//! crate the way a caller does.

use std::path::PathBuf;

use std::sync::{Arc, Mutex};
use syncdash::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use syncdash::fs::vfs::{Medium, ReadStream, VDirEntry, VMeta, VfsCaps, WriteHint, WriteStaged};
use syncdash::model::event::{ItemOutcome, ProgressEvent};
use syncdash::model::plan::{Action, Op, Side};
use syncdash::obs::progress::{RunCtl, RunCtx};
use syncdash::pipeline::apply::{self, ApplyOptions};

fn tmproot(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("syncdash-apply-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn op(action: Action, path: &str) -> Op {
    Op {
        side: Side::Target,
        action,
        path: path.into(),
        from: None,
        size: None,
        mtime_ms: None,
        hash: None,
        link: None,
        mode: None,
        reason: "test".into(),
    }
}

fn move_op(path: &str, from: &str, content: &[u8]) -> Op {
    let mut moving = op(Action::Move, path);
    moving.from = Some(from.into());
    moving.size = Some(content.len() as u64);
    moving.hash = Some(blake3::hash(content).to_hex().to_string());
    moving
}

fn opts(trash: PathBuf) -> ApplyOptions {
    ApplyOptions {
        dry_run: false,
        trash: Some(trash),
        fsync: false,
        ..Default::default()
    }
}

#[test]
fn an_untrusted_plan_cannot_escape_either_root() {
    let base = tmproot("traversal");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    let outside = base.join("outside.txt");
    std::fs::write(&outside, b"must survive").unwrap();

    let out = apply::apply_with(
        &[op(Action::Delete, "../outside.txt")],
        &s,
        &t,
        &opts(base.join("trash")),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(&outside).unwrap(), b"must survive");
    let _ = std::fs::remove_dir_all(&base);
}

/// A ctx that collects ItemResult: the contents of the execution ledger (items.jsonl) are exactly these events
fn ledger_ctx() -> (
    RunCtx,
    std::sync::Arc<Mutex<Vec<(String, ItemOutcome, u64)>>>,
) {
    let store: std::sync::Arc<Mutex<Vec<(String, ItemOutcome, u64)>>> =
        std::sync::Arc::new(Mutex::new(Vec::new()));
    let s2 = store.clone();
    let sink = move |ev: syncdash::model::event::ProgressEvent| {
        if let syncdash::model::event::ProgressEvent::ItemResult {
            path,
            outcome,
            bytes,
            ..
        } = ev
        {
            s2.lock().unwrap().push((path, outcome, bytes));
        }
    };
    (
        RunCtx::new(
            syncdash::obs::progress::RunCtl::new(),
            std::sync::Arc::new(sink),
        ),
        store,
    )
}

#[test]
fn ledger_records_ok_kept_and_failed_per_item() {
    let base = tmproot("ledger");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(t.join("d")).unwrap();
    std::fs::write(s.join("a.txt"), b"hello").unwrap();
    std::fs::write(t.join("d").join("protected.log"), b"x").unwrap();

    let (ctx, log) = ledger_ctx();
    let ops = [
        op(Action::Copy, "a.txt"),       // → ok
        op(Action::DeleteDir, "d"),      // → kept (non-empty and its contents are not deletable)
        op(Action::Copy, "missing.txt"), // → failed (the source file does not exist)
    ];
    let out = apply::apply_with(&ops, &s, &t, &opts(base.join("trash")), &ctx);
    assert_eq!((out.done, out.skipped, out.errors), (1, 1, 1));

    let rows = log.lock().unwrap();
    // Key invariant: every entry in the plan leaves a trace in the ledger — not one more, not one fewer
    assert_eq!(
        rows.len(),
        ops.len(),
        "every op must leave a trace: {rows:?}"
    );
    let find = |p: &str| {
        rows.iter()
            .find(|(path, _, _)| path == p)
            .map(|(_, o, _)| *o)
    };
    assert_eq!(find("a.txt"), Some(ItemOutcome::Ok));
    assert_eq!(
        find("d"),
        Some(ItemOutcome::Kept),
        "keeping the directory is not an error, but it must be traceable"
    );
    assert_eq!(find("missing.txt"), Some(ItemOutcome::Failed));
    drop(rows);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ledger_marks_untouched_items_as_cancelled() {
    let base = tmproot("ledgercancel");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    for i in 0..6 {
        std::fs::write(s.join(format!("f{i}.txt")), vec![b'x'; 4096]).unwrap();
    }
    let (ctx, log) = ledger_ctx();
    ctx.ctl.request_cancel(); // not a single one gets its turn to run
    let ops: Vec<Op> = (0..6)
        .map(|i| op(Action::Copy, &format!("f{i}.txt")))
        .collect();
    let out = apply::apply_with(&ops, &s, &t, &opts(base.join("trash")), &ctx);

    assert_eq!(out.done, 0);
    let rows = log.lock().unwrap();
    // Assert honestly: the checkpoint stopped things **before any work began**, so not a single op ran
    // and the ledger naturally holds no rows. `all()` is vacuously true on an empty set, which would
    // turn this into a false pass — the row count must be pinned explicitly, or a future missing emit in record would go unnoticed.
    assert_eq!(
        rows.len(),
        0,
        "the ledger must be empty when the cancel lands before any op: {rows:?}"
    );
    assert!(rows.iter().all(|(_, o, _)| *o == ItemOutcome::Cancelled));
    drop(rows);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn copy_lands_atomically_and_leaves_no_temp_files() {
    let base = tmproot("copy");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("a.txt"), b"hello").unwrap();

    let (done, _, errors) = apply::apply(
        &[op(Action::Copy, "a.txt")],
        &s,
        &t,
        &opts(base.join("trash")),
    );
    assert_eq!((done, errors), (1, 0));
    assert_eq!(std::fs::read(t.join("a.txt")).unwrap(), b"hello");
    let leftovers: Vec<_> = std::fs::read_dir(&t)
        .unwrap()
        .flatten()
        .filter(|e| syncdash::fs::staged::is_temp_name(&e.file_name().to_string_lossy()))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp files may survive a successful apply"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn failed_update_leaves_the_original_intact() {
    // The source file does not exist → the copy is bound to fail. The destination must be untouched, which is exactly what the atomic write guarantees:
    // fs::copy used to write the destination directly, so a failure left truncated content behind and the next sync propagated it back to source.
    let base = tmproot("fail");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("keep.txt"), b"precious original").unwrap();

    let (done, _, errors) = apply::apply(
        &[op(Action::Update, "keep.txt")],
        &s,
        &t,
        &opts(base.join("trash")),
    );
    assert_eq!(done, 0);
    assert_eq!(errors, 1);
    assert_eq!(
        std::fs::read(t.join("keep.txt")).unwrap(),
        b"precious original",
        "a failed update must never damage the destination"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn update_preserves_old_content_in_trash() {
    let base = tmproot("trash");
    let (s, t, tr) = (base.join("s"), base.join("t"), base.join("trash"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("f.txt"), b"new").unwrap();
    std::fs::write(t.join("f.txt"), b"old").unwrap();

    let (done, _, errors) = apply::apply(&[op(Action::Update, "f.txt")], &s, &t, &opts(tr.clone()));
    assert_eq!((done, errors), (1, 0));
    assert_eq!(std::fs::read(t.join("f.txt")).unwrap(), b"new");
    assert_eq!(
        std::fs::read(tr.join("target/f.txt")).unwrap(),
        b"old",
        "old version must be recoverable"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn central_trash_keeps_source_and_target_originals_in_separate_namespaces() {
    let base = tmproot("trash-sides");
    let (s, t, tr) = (base.join("s"), base.join("t"), base.join("trash"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("same.txt"), b"source original").unwrap();
    std::fs::write(t.join("same.txt"), b"target original").unwrap();
    let mut source_delete = op(Action::Delete, "same.txt");
    source_delete.side = Side::Source;

    let out = apply::apply_with(
        &[source_delete, op(Action::Delete, "same.txt")],
        &s,
        &t,
        &opts(tr.clone()),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (2, 0));
    assert_eq!(
        std::fs::read(tr.join("source/same.txt")).unwrap(),
        b"source original"
    );
    assert_eq!(
        std::fs::read(tr.join("target/same.txt")).unwrap(),
        b"target original"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn duplicate_mutations_are_rejected_before_any_original_is_moved() {
    let base = tmproot("trash-duplicate");
    let (s, t, tr) = (base.join("s"), base.join("t"), base.join("trash"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("same.txt"), b"must survive").unwrap();

    let out = apply::apply_with(
        &[
            op(Action::Delete, "same.txt"),
            op(Action::Delete, "same.txt"),
        ],
        &s,
        &t,
        &opts(tr.clone()),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(t.join("same.txt")).unwrap(), b"must survive");
    assert!(!tr.exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn duplicate_move_sources_are_rejected_before_the_first_source_is_claimed() {
    let base = tmproot("duplicate-move-source");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"planned source").unwrap();

    let out = apply::apply_with(
        &[
            move_op("one.txt", "old.txt", b"planned source"),
            move_op("two.txt", "old.txt", b"planned source"),
        ],
        &s,
        &t,
        &opts(base.join("trash")),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(t.join("old.txt")).unwrap(), b"planned source");
    assert!(!t.join("one.txt").exists());
    assert!(!t.join("two.txt").exists());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn move_without_source_evidence_is_rejected_before_claiming_the_name() {
    let base = tmproot("move-missing-evidence");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"must survive").unwrap();
    let mut moving = op(Action::Move, "new.txt");
    moving.from = Some("old.txt".into());

    let out = apply::apply_with(
        &[moving],
        &s,
        &t,
        &opts(base.join("trash")),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(t.join("old.txt")).unwrap(), b"must survive");
    assert!(!t.join("new.txt").exists());
    assert!(move_temp_files(&t).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn mutation_of_a_move_source_is_rejected_independent_of_plan_order() {
    for reverse in [false, true] {
        let base = tmproot(if reverse {
            "move-source-mutation-reverse"
        } else {
            "move-source-mutation"
        });
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("old.txt"), b"planned source").unwrap();
        let mut moving = move_op("new.txt", "old.txt", b"planned source");
        moving.reason = "conflict-loser-kept-as-copy (spoofed)".into();
        let mut deleting = op(Action::Delete, "old.txt");
        deleting.reason = "conflict-resolved-newer-wins (loser kept as .sync-conflict copy)".into();
        let ops = if reverse {
            vec![deleting, moving]
        } else {
            vec![moving, deleting]
        };

        let out = apply::apply_with(&ops, &s, &t, &opts(base.join("trash")), &RunCtx::null());
        assert_eq!((out.done, out.errors), (0, 1));
        assert_eq!(std::fs::read(t.join("old.txt")).unwrap(), b"planned source");
        assert!(!t.join("new.txt").exists());
        let _ = std::fs::remove_dir_all(base);
    }
}

#[test]
fn planner_conflict_copy_may_move_the_loser_then_recreate_its_old_name() {
    let base = tmproot("conflict-move-recreate");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("doc.txt"), b"winner").unwrap();
    std::fs::write(t.join("doc.txt"), b"loser").unwrap();

    let mut keep = move_op("doc.sync-conflict-host.txt", "doc.txt", b"loser");
    keep.reason = "conflict-loser-kept-as-copy (both-changed)".into();
    let mut recreate = op(Action::Update, "doc.txt");
    recreate.size = Some(6);
    recreate.reason = "conflict-resolved-newer-wins (loser kept as .sync-conflict copy)".into();

    let out = apply::apply_with(
        // Serialized in the hostile order to pin the scheduler's class ordering: Move still
        // consumes the loser before Update reads the opposite root and recreates this name.
        &[recreate, keep],
        &s,
        &t,
        &opts(base.join("trash")),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (2, 0));
    assert_eq!(std::fs::read(t.join("doc.txt")).unwrap(), b"winner");
    assert_eq!(
        std::fs::read(t.join("doc.sync-conflict-host.txt")).unwrap(),
        b"loser"
    );
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn move_cycles_are_rejected_before_any_name_changes() {
    let base = tmproot("move-cycle");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("a.txt"), b"a").unwrap();
    std::fs::write(t.join("b.txt"), b"b").unwrap();

    let out = apply::apply_with(
        &[
            move_op("b.txt", "a.txt", b"a"),
            move_op("a.txt", "b.txt", b"b"),
        ],
        &s,
        &t,
        &opts(base.join("trash")),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(t.join("a.txt")).unwrap(), b"a");
    assert_eq!(std::fs::read(t.join("b.txt")).unwrap(), b"b");
    assert!(move_temp_files(&t).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn move_refuses_a_destination_that_appeared_after_compare() {
    let base = tmproot("move-destination-drift");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"planned source").unwrap();
    std::fs::write(t.join("new.txt"), b"post-compare occupant").unwrap();
    let moving = move_op("new.txt", "old.txt", b"planned source");

    let out = apply::apply_with(
        &[moving],
        &s,
        &t,
        &opts(base.join("trash")),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(t.join("old.txt")).unwrap(), b"planned source");
    assert_eq!(
        std::fs::read(t.join("new.txt")).unwrap(),
        b"post-compare occupant"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn durable_same_volume_move_syncs_both_parent_directories() {
    let base = tmproot("move-parent-fsync");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(t.join("before")).unwrap();
    std::fs::create_dir_all(t.join("after")).unwrap();
    std::fs::write(t.join("before/old.txt"), b"planned source").unwrap();
    let mut options = opts(base.join("trash"));
    options.fsync = true;

    let out = apply::apply_with(
        &[move_op(
            "after/new.txt",
            "before/old.txt",
            b"planned source",
        )],
        &s,
        &t,
        &options,
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (1, 0));
    assert!(!t.join("before/old.txt").exists());
    assert_eq!(
        std::fs::read(t.join("after/new.txt")).unwrap(),
        b"planned source"
    );
    assert!(move_temp_files(&t.join("before")).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn near_name_max_move_source_uses_a_bounded_hold_name() {
    let base = tmproot("move-long-basename");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    let from = "a".repeat(240);
    let to = "b".repeat(240);
    std::fs::write(t.join(&from), b"planned source").unwrap();

    let out = apply::apply_with(
        &[move_op(&to, &from, b"planned source")],
        &s,
        &t,
        &opts(base.join("trash")),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (1, 0));
    assert!(!t.join(from).exists());
    assert_eq!(std::fs::read(t.join(to)).unwrap(), b"planned source");
    assert!(move_temp_files(&t).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn same_volume_symlink_move_verifies_the_link_itself() {
    let base = tmproot("move-symlink");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::os::unix::fs::symlink("relative-target", t.join("old-link")).unwrap();
    let mut moving = op(Action::Move, "new-link");
    moving.from = Some("old-link".into());
    moving.size = Some(0);
    moving.link = Some("relative-target".into());

    let out = apply::apply_with(
        &[moving],
        &s,
        &t,
        &opts(base.join("trash")),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (1, 0));
    assert!(std::fs::symlink_metadata(t.join("old-link")).is_err());
    assert_eq!(
        std::fs::read_link(t.join("new-link")).unwrap(),
        std::path::PathBuf::from("relative-target")
    );
    assert!(move_temp_files(&t).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn readonly_files_delete_and_update_like_git_objects() {
    // Git marks loose objects r--r--r--; Windows refuses to delete read-only files,
    // which a live sync surfaced as thousands of os-error-5 Delete failures.
    // Both the delete and the overwrite lane must clear the attribute and proceed.
    let base = tmproot("readonly");
    let (s, t, tr) = (base.join("s"), base.join("t"), base.join("trash"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("upd.bin"), b"new").unwrap();
    for name in ["gone.bin", "upd.bin"] {
        let p = t.join(name);
        std::fs::write(&p, b"old").unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&p, perms).unwrap();
    }

    let (done, _, errors) = apply::apply(
        &[
            op(Action::Delete, "gone.bin"),
            op(Action::Update, "upd.bin"),
        ],
        &s,
        &t,
        &opts(tr.clone()),
    );
    assert_eq!(errors, 0, "read-only originals must not fail the ops");
    assert_eq!(done, 2);
    assert!(
        !t.join("gone.bin").exists(),
        "the read-only file must really be gone"
    );
    assert_eq!(std::fs::read(t.join("upd.bin")).unwrap(), b"new");
    // and both originals are still recoverable from the trash
    assert_eq!(std::fs::read(tr.join("target/gone.bin")).unwrap(), b"old");
    assert_eq!(std::fs::read(tr.join("target/upd.bin")).unwrap(), b"old");

    // The assertions above pass on unix without exercising anything: `rm` of a 0444 file succeeds,
    // so the retry never fires and "errors == 0" proves nothing about the code this test names.
    // What matters here is the end state — the mode must come through untouched. The retry used to
    // be unconditional, and on unix `set_readonly(false)` is `mode |= 0o222`, so a file that took
    // the retry path was left group- and world-writable for good.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(tr.join("target/gone.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o444,
            "preserving a read-only file must not widen it (0o{mode:o}); on unix the retry is not the remedy and must not run"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn delete_dir_reports_kept_contents_instead_of_silence() {
    let base = tmproot("deldir");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(t.join("d")).unwrap();
    std::fs::write(t.join("d").join("protected.log"), b"x").unwrap();

    // No filter → the leftovers are not deletable → count it as skipped and print the reason, rather than pretending it succeeded
    let (done, skipped, errors) = apply::apply(
        &[op(Action::DeleteDir, "d")],
        &s,
        &t,
        &opts(base.join("trash")),
    );
    assert_eq!(done, 0, "the directory was not actually removed");
    assert_eq!(
        skipped, 1,
        "it must be reported, not silently counted as done"
    );
    assert_eq!(errors, 0, "keeping a protected file is not an error");
    assert!(t.join("d").is_dir());
    let _ = std::fs::remove_dir_all(&base);
}

/// A symlink is content, and it used to be the one kind of leftover that could not protect its
/// directory. Symlinks were pushed onto the removal list without being counted or tested for
/// deletability, so a directory whose leftovers were *only* symlinks had count == 0, skipped the
/// "not empty, and not everything here is disposable" guard, and had every link unlinked directly —
/// no preserve, no version store, no trash. With `symlinks = "exclude"` (the default) the links are
/// also absent from both snapshots, so nothing recorded what was destroyed. One `Foo.app` was
/// enough: every `Versions/Current -> A` inside it went this way.
#[cfg(unix)]
#[test]
fn delete_dir_will_not_silently_unlink_symlinks() {
    let base = tmproot("deldir-link");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(t.join("d")).unwrap();
    // The leftovers must be symlinks and nothing else. A single regular file beside them would
    // protect the directory on its own and the test would pass without proving anything.
    std::fs::write(t.join("payload.txt"), b"payload").unwrap();
    std::os::unix::fs::symlink("../payload.txt", t.join("d").join("Current")).unwrap();
    std::os::unix::fs::symlink("../payload.txt", t.join("d").join("Latest")).unwrap();

    let (done, skipped, errors) = apply::apply(
        &[op(Action::DeleteDir, "d")],
        &s,
        &t,
        &opts(base.join("trash")),
    );
    assert_eq!(
        done, 0,
        "the directory must not be removed over a leftover symlink"
    );
    assert_eq!(
        skipped, 1,
        "it must be reported, exactly as a protected file is"
    );
    assert_eq!(errors, 0);
    assert!(
        t.join("d").join("Current").symlink_metadata().is_ok(),
        "the symlink must still be there"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn delete_dir_removes_when_leftovers_are_deletable() {
    let base = tmproot("deldir2");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(t.join("d")).unwrap();
    std::fs::write(t.join("d").join("cache.tmp"), b"x").unwrap();

    let mut o = opts(base.join("trash"));
    o.filter = Some(syncdash::pipeline::filter::PathFilter::build_full(
        &[],
        &[],
        &["*/*.tmp".to_string()],
    ));
    let (done, _, errors) = apply::apply(&[op(Action::DeleteDir, "d")], &s, &t, &o);
    assert_eq!((done, errors), (1, 0));
    assert!(
        !t.join("d").exists(),
        "deletable leftovers must not block the directory removal"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn delta_update_produces_identical_content() {
    let base = tmproot("delta");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    // Larger than DELTA_MIN_SIZE, with only a short stretch in the middle differing
    let mut old = vec![0u8; syncdash::model::chunk::DELTA_MIN_SIZE as usize + 256 * 1024];
    for (i, b) in old.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let mut new = old.clone();
    new[3_000_000..3_001_000].fill(0xAB);
    std::fs::write(s.join("big.bin"), &new).unwrap();
    std::fs::write(t.join("big.bin"), &old).unwrap();

    let mut o = opts(base.join("trash"));
    o.delta = true;
    let (done, _, errors) = apply::apply(&[op(Action::Update, "big.bin")], &s, &t, &o);
    assert_eq!((done, errors), (1, 0));
    assert_eq!(
        std::fs::read(t.join("big.bin")).unwrap(),
        new,
        "delta path must be byte-exact"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn delta_update_handles_shrinking_files() {
    let base = tmproot("delta2");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    let old = vec![7u8; 5 * 1024 * 1024];
    let new = vec![7u8; syncdash::model::chunk::DELTA_MIN_SIZE as usize + 64 * 1024];
    std::fs::write(s.join("shrink.bin"), &new).unwrap();
    std::fs::write(t.join("shrink.bin"), &old).unwrap();

    let mut o = opts(base.join("trash"));
    o.delta = true;
    let (done, _, errors) = apply::apply(&[op(Action::Update, "shrink.bin")], &s, &t, &o);
    assert_eq!((done, errors), (1, 0));
    assert_eq!(
        std::fs::read(t.join("shrink.bin")).unwrap().len(),
        new.len(),
        "tail must be truncated"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// The paranoid tier on a local root. Both lanes used to read the staged file back through
/// `update_mmap_rayon`; both now use a chunked read, and neither had a test. The payloads are
/// deliberately larger than the 8 MiB read granularity so the loop runs more than once — a
/// single-shot read would satisfy a smaller fixture either way.
#[test]
fn verify_reads_the_staged_file_back_in_both_lanes() {
    let base = tmproot("verify");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();

    let mut fresh = vec![0u8; 10 * 1024 * 1024];
    for (i, b) in fresh.iter_mut().enumerate() {
        *b = (i % 253) as u8;
    }
    // The delta lane needs an existing target that differs from the source only in a short stretch
    let mut updated = fresh.clone();
    updated[7_000_000..7_002_000].fill(0xCD);
    std::fs::write(s.join("new.bin"), &fresh).unwrap();
    std::fs::write(s.join("patched.bin"), &updated).unwrap();
    std::fs::write(t.join("patched.bin"), &fresh).unwrap();

    let mut o = opts(base.join("trash"));
    o.verify = true;
    o.delta = true;
    let ops = [
        op(Action::Copy, "new.bin"),
        op(Action::Update, "patched.bin"),
    ];
    let (done, _, errors) = apply::apply(&ops, &s, &t, &o);

    assert_eq!(
        (done, errors),
        (2, 0),
        "a readback matching the copy stream is not an error"
    );
    assert_eq!(
        std::fs::read(t.join("new.bin")).unwrap(),
        fresh,
        "generic lane must land byte-exact under verify"
    );
    assert_eq!(
        std::fs::read(t.join("patched.bin")).unwrap(),
        updated,
        "delta lane must land byte-exact under verify"
    );
    let _ = std::fs::remove_dir_all(&base);
}

fn collecting_ctx() -> (RunCtx, Arc<Mutex<Vec<ProgressEvent>>>) {
    let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let s2 = store.clone();
    let sink = move |ev: ProgressEvent| {
        s2.lock().unwrap().push(ev);
    };
    (RunCtx::new(RunCtl::new(), Arc::new(sink)), store)
}

/// Parallel and sequential must produce byte-identical result trees (including the set preserved in trash)
#[test]
fn parallel_and_sequential_agree() {
    let run = |tag: &str, parallel: usize| -> (PathBuf, PathBuf, (u64, u64, u64)) {
        let base = tmproot(&format!("par-{tag}"));
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        let mut ops = Vec::new();
        for i in 0..12 {
            let name = format!("f{i:02}.bin");
            std::fs::write(s.join(&name), vec![i as u8; 20_000 + i * 1000]).unwrap();
            ops.push(op(Action::Copy, &name));
        }
        // Mix in one update (old content goes to trash) and one delete
        std::fs::write(s.join("up.txt"), b"NEW").unwrap();
        std::fs::write(t.join("up.txt"), b"OLD").unwrap();
        ops.push(op(Action::Update, "up.txt"));
        std::fs::write(t.join("gone.txt"), b"bye").unwrap();
        ops.push(op(Action::Delete, "gone.txt"));

        let mut o = opts(base.join("trash"));
        o.parallel = parallel;
        let r = apply::apply(&ops, &s, &t, &o);
        (base.clone(), t, r)
    };
    let (b1, t1, r1) = run("seq", 1);
    let (b2, t2, r2) = run("par", 4);
    assert_eq!(r1, r2, "counts must match");
    let user_files = |root: &std::path::Path| {
        std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|entry| {
                !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(syncdash::foundation::names::LOCK_NAME)
            })
            .map(|entry| {
                let name = entry.file_name();
                let bytes = std::fs::read(entry.path()).unwrap();
                (name, bytes)
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(
        user_files(&t1),
        user_files(&t2),
        "parallel and sequential runs must produce the same user-visible files"
    );
    let _ = std::fs::remove_dir_all(&b1);
    let _ = std::fs::remove_dir_all(&b2);
}

/// Errors don't abort: 10 good, 1 bad → 10 applied + 1 Error event, byte ledger = Σ of the successful files
#[test]
fn errors_accumulate_without_aborting() {
    let base = tmproot("errs");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    let mut ops = Vec::new();
    let mut expect_bytes = 0u64;
    for i in 0..10 {
        let name = format!("g{i}.bin");
        let body = vec![9u8; 5_000];
        std::fs::write(s.join(&name), &body).unwrap();
        expect_bytes += body.len() as u64;
        let mut o = op(Action::Copy, &name);
        o.size = Some(body.len() as u64);
        ops.push(o);
    }
    ops.push(op(Action::Copy, "missing-on-source.bin")); // guaranteed to fail

    let (ctx, store) = collecting_ctx();
    let out = apply::apply_with(&ops, &s, &t, &opts(base.join("trash")), &ctx);
    assert_eq!(out.done, 10);
    assert_eq!(out.errors, 1);
    assert!(!out.cancelled);
    assert_eq!(
        out.bytes_copied, expect_bytes,
        "byte ledger must equal the sum of successful copies"
    );
    let evs = store.lock().unwrap();
    assert_eq!(
        evs.iter()
            .filter(|e| matches!(e, ProgressEvent::Error { .. }))
            .count(),
        1,
        "exactly one Error event"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Cancel mid-copy of a big file: honored between chunks, no half file at the destination, zero .syncdash.tmp debris
#[test]
fn cancel_mid_copy_leaves_no_debris() {
    let base = tmproot("cancel");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("big.bin"), vec![3u8; 2 * 1024 * 1024]).unwrap();
    let mut o = op(Action::Copy, "big.bin");
    o.size = Some(2 * 1024 * 1024);

    let ctl = RunCtl::new();
    let ctl2 = ctl.clone();
    // Request the cancel right after the first Progress event (= the first 1MiB chunk)
    let sink = move |ev: ProgressEvent| {
        if matches!(ev, ProgressEvent::Progress { .. }) {
            ctl2.request_cancel();
        }
    };
    let ctx = RunCtx::new(ctl, Arc::new(sink));
    let out = apply::apply_with(&[o], &s, &t, &opts(base.join("trash")), &ctx);
    assert!(out.cancelled);
    assert_eq!(out.done, 0);
    assert_eq!(out.errors, 0, "cancellation is not an error");
    assert!(
        !t.join("big.bin").exists(),
        "no partial file may reach the destination"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&t)
        .unwrap()
        .flatten()
        .filter(|e| syncdash::fs::staged::is_temp_name(&e.file_name().to_string_lossy()))
        .collect();
    assert!(leftovers.is_empty(), "temp files must be cleaned on cancel");
    let _ = std::fs::remove_dir_all(&base);
}

/// DeleteDir depth ordering: subdirectories go before their parent, so the parent has a chance to be empty
#[test]
fn delete_dirs_deepest_first_regardless_of_input_order() {
    let base = tmproot("depth");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(t.join("a").join("b").join("c")).unwrap();
    // Deliberately hand over the ops in the wrong shallow→deep order
    let ops = vec![
        op(Action::DeleteDir, "a"),
        op(Action::DeleteDir, "a/b"),
        op(Action::DeleteDir, "a/b/c"),
    ];
    let (done, _, errors) = apply::apply(&ops, &s, &t, &opts(base.join("trash")));
    assert_eq!((done, errors), (3, 0), "all three directories must go");
    assert!(!t.join("a").exists());
    let _ = std::fs::remove_dir_all(&base);
}

/// A route fixture backed by real files. It can hide its local capability to model a protocol
/// root, or expose it while denying `local_trash` to model a local root that cannot reach the
/// default store. That distinction pins routing without depending on mounted media.
struct RouteFixture(syncdash::fs::vfs::local::LocalVfs, Medium, bool);

impl syncdash::fs::vfs::Vfs for RouteFixture {
    fn caps(&self) -> VfsCaps {
        VfsCaps {
            medium: self.1,
            local_trash: false,
            ..self.0.caps()
        }
    }
    fn display(&self) -> String {
        self.0.display()
    }
    fn identity(&self) -> String {
        self.0.identity()
    }
    fn local_root(&self) -> Option<&syncdash::fs::local_root::LocalRoot> {
        if self.2 {
            Some(self.0.local_root())
        } else {
            None
        }
    }
    fn connect(&self) -> VfsResult<()> {
        self.0.connect()
    }
    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        self.0.stat(rel)
    }
    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        self.0.read_dir(rel)
    }
    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        self.0.open_read(rel)
    }
    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        self.0.read_range(rel, off, len)
    }
    fn read_link(&self, rel: &str) -> VfsResult<String> {
        self.0.read_link(rel)
    }
    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        self.0.mkdir_all(rel)
    }
    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        self.0.open_write(rel, hint)
    }
    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        self.0.rename(from_rel, to_rel)
    }
    fn rename_noreplace(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        self.0.rename_noreplace(from_rel, to_rel)
    }
    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        self.0.remove_file(rel)
    }
    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        self.0.remove_dir(rel)
    }
    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        self.0.set_mtime(rel, mtime_ms)
    }
    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        self.0.set_mode(rel, mode)
    }
    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        self.0.make_symlink(rel, target)
    }
    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        self.0.free_space()
    }
}

struct RenameDrift(RouteFixture);

impl syncdash::fs::vfs::Vfs for RenameDrift {
    fn caps(&self) -> VfsCaps {
        self.0.caps()
    }
    fn display(&self) -> String {
        self.0.display()
    }
    fn identity(&self) -> String {
        self.0.identity()
    }
    fn local_root(&self) -> Option<&syncdash::fs::local_root::LocalRoot> {
        self.0.local_root()
    }
    fn connect(&self) -> VfsResult<()> {
        self.0.connect()
    }
    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        self.0.stat(rel)
    }
    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        self.0.read_dir(rel)
    }
    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        self.0.open_read(rel)
    }
    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        self.0.read_range(rel, off, len)
    }
    fn read_link(&self, rel: &str) -> VfsResult<String> {
        self.0.read_link(rel)
    }
    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        self.0.mkdir_all(rel)
    }
    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        self.0.open_write(rel, hint)
    }
    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        if from_rel == "old.txt" && to_rel == "new.txt" {
            std::fs::write(
                self.local_root().unwrap().display_path().join(to_rel),
                b"raced occupant",
            )?;
            return Err(VfsError::new(
                VfsErrorKind::Io,
                "forced rename fallback after destination drift",
            ));
        }
        self.0.rename(from_rel, to_rel)
    }
    fn rename_noreplace(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        if syncdash::fs::staged::is_temp_rel(from_rel) && to_rel == "new.txt" {
            std::fs::write(
                self.local_root().unwrap().display_path().join(to_rel),
                b"raced occupant",
            )?;
        }
        self.0.rename_noreplace(from_rel, to_rel)
    }
    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        self.0.remove_file(rel)
    }
    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        self.0.remove_dir(rel)
    }
    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        self.0.set_mtime(rel, mtime_ms)
    }
    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        self.0.set_mode(rel, mode)
    }
    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        self.0.make_symlink(rel, target)
    }
    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        self.0.free_space()
    }
}

#[derive(Clone, Copy)]
enum MoveFault {
    ReplaceOriginalDuringCopy,
    TruncateCopyStream,
    RefuseHoldCleanup,
    DriftAfterClaim,
    DriftDuringSameVolumePublish,
    CrossDeviceOnly,
}

struct FaultMoveVfs {
    inner: RouteFixture,
    root: PathBuf,
    fault: MoveFault,
    hold_opens: std::sync::atomic::AtomicUsize,
}

impl FaultMoveVfs {
    fn new(root: PathBuf, fault: MoveFault) -> FaultMoveVfs {
        FaultMoveVfs {
            inner: RouteFixture(
                syncdash::fs::vfs::local::LocalVfs::open(root.clone()).unwrap(),
                Medium::FixedDisk,
                true,
            ),
            root,
            fault,
            hold_opens: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

struct FaultRead {
    inner: Box<dyn ReadStream>,
    replacement_root: Option<PathBuf>,
    remaining: Option<u64>,
    fired: bool,
}

impl std::io::Read for FaultRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == Some(0) {
            return Ok(0);
        }
        let width = self.remaining.map_or(buf.len(), |remaining| {
            remaining.min(buf.len() as u64) as usize
        });
        let n = std::io::Read::read(&mut self.inner, &mut buf[..width])?;
        if let Some(remaining) = &mut self.remaining {
            *remaining = remaining.saturating_sub(n as u64);
        }
        if n == 0 && !self.fired {
            self.fired = true;
            if let Some(root) = &self.replacement_root {
                std::fs::write(root.join("old.txt"), b"replacement writer")?;
            }
        }
        Ok(n)
    }
}

impl ReadStream for FaultRead {
    fn block_size(&self) -> usize {
        self.inner.block_size()
    }
}

impl syncdash::fs::vfs::Vfs for FaultMoveVfs {
    fn caps(&self) -> VfsCaps {
        self.inner.caps()
    }
    fn display(&self) -> String {
        self.inner.display()
    }
    fn identity(&self) -> String {
        self.inner.identity()
    }
    fn local_root(&self) -> Option<&syncdash::fs::local_root::LocalRoot> {
        self.inner.local_root()
    }
    fn connect(&self) -> VfsResult<()> {
        self.inner.connect()
    }
    fn stat(&self, rel: &str) -> VfsResult<Option<VMeta>> {
        self.inner.stat(rel)
    }
    fn read_dir(&self, rel: &str) -> VfsResult<Vec<VDirEntry>> {
        self.inner.read_dir(rel)
    }
    fn open_read(&self, rel: &str) -> VfsResult<Box<dyn ReadStream>> {
        let inner = self.inner.open_read(rel)?;
        if !syncdash::fs::staged::is_temp_rel(rel) {
            return Ok(inner);
        }
        let call = self
            .hold_opens
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call != 1 {
            return Ok(inner); // first open verifies evidence; second is the fallback copy
        }
        match self.fault {
            MoveFault::ReplaceOriginalDuringCopy => Ok(Box::new(FaultRead {
                inner,
                replacement_root: Some(self.root.clone()),
                remaining: None,
                fired: false,
            })),
            MoveFault::TruncateCopyStream => Ok(Box::new(FaultRead {
                inner,
                replacement_root: None,
                remaining: Some(4),
                fired: false,
            })),
            _ => Ok(inner),
        }
    }
    fn read_range(&self, rel: &str, off: u64, len: u32) -> VfsResult<Vec<u8>> {
        self.inner.read_range(rel, off, len)
    }
    fn read_link(&self, rel: &str) -> VfsResult<String> {
        self.inner.read_link(rel)
    }
    fn mkdir_all(&self, rel: &str) -> VfsResult<()> {
        self.inner.mkdir_all(rel)
    }
    fn open_write(&self, rel: &str, hint: &WriteHint) -> VfsResult<Box<dyn WriteStaged>> {
        self.inner.open_write(rel, hint)
    }
    fn rename(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        self.inner.rename(from_rel, to_rel)
    }
    fn rename_noreplace(&self, from_rel: &str, to_rel: &str) -> VfsResult<()> {
        if syncdash::fs::staged::is_temp_rel(from_rel)
            && to_rel == "new.txt"
            && !matches!(self.fault, MoveFault::DriftDuringSameVolumePublish)
        {
            return Err(VfsError::new(
                VfsErrorKind::CrossDevice,
                "injected cross-volume move",
            ));
        }
        let result = self.inner.rename_noreplace(from_rel, to_rel);
        if result.is_ok()
            && from_rel == "old.txt"
            && syncdash::fs::staged::is_temp_rel(to_rel)
            && matches!(self.fault, MoveFault::DriftAfterClaim)
        {
            std::fs::write(self.root.join(to_rel), b"external drift")?;
        }
        if result.is_ok()
            && syncdash::fs::staged::is_temp_rel(from_rel)
            && to_rel == "new.txt"
            && matches!(self.fault, MoveFault::DriftDuringSameVolumePublish)
        {
            std::fs::write(self.root.join(to_rel), b"publish-window drift")?;
        }
        result
    }
    fn remove_file(&self, rel: &str) -> VfsResult<()> {
        if syncdash::fs::staged::is_temp_rel(rel)
            && matches!(self.fault, MoveFault::RefuseHoldCleanup)
        {
            return Err(VfsError::new(
                VfsErrorKind::PermissionDenied,
                "injected move-hold cleanup refusal",
            ));
        }
        self.inner.remove_file(rel)
    }
    fn remove_dir(&self, rel: &str) -> VfsResult<()> {
        self.inner.remove_dir(rel)
    }
    fn set_mtime(&self, rel: &str, mtime_ms: i64) -> VfsResult<()> {
        self.inner.set_mtime(rel, mtime_ms)
    }
    fn set_mode(&self, rel: &str, mode: u32) -> VfsResult<()> {
        self.inner.set_mode(rel, mode)
    }
    fn make_symlink(&self, rel: &str, target: &str) -> VfsResult<()> {
        self.inner.make_symlink(rel, target)
    }
    fn free_space(&self) -> VfsResult<Option<(u64, u64)>> {
        self.inner.free_space()
    }
}

#[test]
fn move_fallback_rechecks_destination_before_staging_a_copy() {
    let base = tmproot("move-fallback-drift");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"planned source").unwrap();
    let source: Arc<dyn syncdash::fs::vfs::Vfs> =
        Arc::new(syncdash::fs::vfs::local::LocalVfs::open(s).unwrap());
    let target: Arc<dyn syncdash::fs::vfs::Vfs> = Arc::new(RenameDrift(RouteFixture(
        syncdash::fs::vfs::local::LocalVfs::open(t.clone()).unwrap(),
        Medium::FixedDisk,
        true,
    )));
    let moving = move_op("new.txt", "old.txt", b"planned source");

    let out = apply::apply_vfs(
        &[moving],
        &source,
        &target,
        &opts(base.join("trash")),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(t.join("old.txt")).unwrap(), b"planned source");
    assert_eq!(std::fs::read(t.join("new.txt")).unwrap(), b"raced occupant");
    let _ = std::fs::remove_dir_all(&base);
}

fn move_temp_files(root: &std::path::Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .filter(|entry| syncdash::fs::staged::is_temp_name(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect()
}

fn run_fault_move(
    base: &std::path::Path,
    fault: MoveFault,
    moving: Op,
) -> syncdash::obs::progress::ApplyOutcome {
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    let source: Arc<dyn syncdash::fs::vfs::Vfs> =
        Arc::new(syncdash::fs::vfs::local::LocalVfs::open(s).unwrap());
    let target: Arc<dyn syncdash::fs::vfs::Vfs> = Arc::new(FaultMoveVfs::new(t, fault));
    apply::apply_vfs(
        &[moving],
        &source,
        &target,
        &opts(base.join("trash")),
        &RunCtx::null(),
    )
}

#[test]
fn replacement_at_the_original_name_survives_cross_volume_move_cleanup() {
    let base = tmproot("move-source-replacement");
    let t = base.join("t");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"planned source").unwrap();

    let out = run_fault_move(
        &base,
        MoveFault::ReplaceOriginalDuringCopy,
        move_op("new.txt", "old.txt", b"planned source"),
    );

    assert_eq!((out.done, out.errors), (1, 0));
    assert_eq!(std::fs::read(t.join("new.txt")).unwrap(), b"planned source");
    assert_eq!(
        std::fs::read(t.join("old.txt")).unwrap(),
        b"replacement writer",
        "cleanup must unlink only the claimed hold, never a replacement at the original path"
    );
    assert!(move_temp_files(&t).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn a_truncated_fallback_stream_cannot_publish_and_restores_the_claim() {
    let base = tmproot("move-truncated-stream");
    let t = base.join("t");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"planned source").unwrap();

    let out = run_fault_move(
        &base,
        MoveFault::TruncateCopyStream,
        move_op("new.txt", "old.txt", b"planned source"),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(t.join("old.txt")).unwrap(), b"planned source");
    assert!(!t.join("new.txt").exists());
    assert!(move_temp_files(&t).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn failed_hold_cleanup_reports_a_recoverable_duplicate_after_publication() {
    let base = tmproot("move-hold-cleanup");
    let t = base.join("t");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"planned source").unwrap();

    let out = run_fault_move(
        &base,
        MoveFault::RefuseHoldCleanup,
        move_op("new.txt", "old.txt", b"planned source"),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(t.join("new.txt")).unwrap(), b"planned source");
    assert!(!t.join("old.txt").exists());
    let holds = move_temp_files(&t);
    assert_eq!(holds.len(), 1, "the source hold is the recoverable copy");
    assert_eq!(std::fs::read(&holds[0]).unwrap(), b"planned source");
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn same_volume_source_drift_fails_before_destination_publication() {
    let base = tmproot("move-drift-after-claim");
    let t = base.join("t");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"planned source").unwrap();

    let out = run_fault_move(
        &base,
        MoveFault::DriftAfterClaim,
        move_op("new.txt", "old.txt", b"planned source"),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(std::fs::read(t.join("old.txt")).unwrap(), b"external drift");
    assert!(!t.join("new.txt").exists());
    assert!(move_temp_files(&t).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn same_volume_publish_window_drift_is_detected_and_rolled_back() {
    let base = tmproot("move-drift-during-publish");
    let t = base.join("t");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"planned source").unwrap();

    let out = run_fault_move(
        &base,
        MoveFault::DriftDuringSameVolumePublish,
        move_op("new.txt", "old.txt", b"planned source"),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert_eq!(
        std::fs::read(t.join("old.txt")).unwrap(),
        b"publish-window drift"
    );
    assert!(!t.join("new.txt").exists());
    assert!(move_temp_files(&t).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn cross_volume_move_fallback_preserves_executable_mode() {
    use std::os::unix::fs::PermissionsExt;

    let base = tmproot("move-mode");
    let t = base.join("t");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old.txt"), b"planned source").unwrap();
    std::fs::set_permissions(t.join("old.txt"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut moving = move_op("new.txt", "old.txt", b"planned source");
    moving.mode = Some(0o755);

    let out = run_fault_move(&base, MoveFault::CrossDeviceOnly, moving);

    assert_eq!((out.done, out.errors), (1, 0));
    assert_eq!(
        std::fs::metadata(t.join("new.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert!(!t.join("old.txt").exists());
    assert!(move_temp_files(&t).is_empty());
    let _ = std::fs::remove_dir_all(base);
}

/// The bug: preserving an original chose its route from local capability presence, so a share took the central
/// trash store, the same-volume rename into it failed, and `move_to_trash` fell back to
/// `fs::copy` — **downloading every deleted file** before removing it. A mirror clearing 50 GB
/// off a NAS pulled 50 GB onto the local disk, and the space gate had checked the share.
#[test]
fn an_off_machine_root_preserves_in_place_instead_of_downloading() {
    let base = tmproot("offmachine");
    let (s, t, tr) = (base.join("s"), base.join("t"), base.join("trash"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("f.txt"), b"new").unwrap();
    std::fs::write(t.join("f.txt"), b"old").unwrap();

    let sv: Arc<dyn syncdash::fs::vfs::Vfs> =
        Arc::new(syncdash::fs::vfs::local::LocalVfs::open(s.clone()).unwrap());
    let tv: Arc<dyn syncdash::fs::vfs::Vfs> = Arc::new(RouteFixture(
        syncdash::fs::vfs::local::LocalVfs::open(t.clone()).unwrap(),
        Medium::NetworkShare,
        false,
    ));
    let out = apply::apply_vfs(
        &[op(Action::Update, "f.txt")],
        &sv,
        &tv,
        &opts(tr.clone()),
        &RunCtx::null(),
    );
    assert_eq!((out.done, out.errors), (1, 0));
    assert_eq!(std::fs::read(t.join("f.txt")).unwrap(), b"new");

    // Nothing crossed the link: the central store never sees this root's originals
    assert!(
        !tr.join("target/f.txt").exists(),
        "the original must not be copied off the root"
    );

    // …it was renamed into the root's own retention area instead, recoverable with any browser
    let kept = std::fs::read_dir(t.join(".syncdash").join("trash"))
        .expect("in-root retention area must exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("f.txt"))
        .find(|p| p.exists())
        .expect("the original must be kept under <root>/.syncdash/trash/<run>/");
    assert_eq!(
        std::fs::read(kept).unwrap(),
        b"old",
        "old version must still be recoverable"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_protocol_root_reports_the_in_root_preservation_path() {
    let base = tmproot("external-retention-report");
    let (s, t, tr) = (
        base.join("s"),
        base.join("external"),
        base.join("central-trash"),
    );
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("f.txt"), b"new").unwrap();
    std::fs::write(t.join("f.txt"), b"old").unwrap();

    let sv: Arc<dyn syncdash::fs::vfs::Vfs> =
        Arc::new(syncdash::fs::vfs::local::LocalVfs::open(s).unwrap());
    let tv: Arc<dyn syncdash::fs::vfs::Vfs> = Arc::new(RouteFixture(
        syncdash::fs::vfs::local::LocalVfs::open(t.clone()).unwrap(),
        Medium::RemovableDisk,
        false,
    ));
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let copy = events.clone();
    let ctx = RunCtx::new(
        RunCtl::new(),
        Arc::new(move |event| copy.lock().unwrap().push(event)),
    );
    let out = apply::apply_vfs(
        &[op(Action::Update, "f.txt")],
        &sv,
        &tv,
        &opts(tr.clone()),
        &ctx,
    );
    assert_eq!((out.done, out.errors), (1, 0));

    let messages: Vec<String> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            ProgressEvent::Log { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect();
    let report = messages
        .iter()
        .find(|message| message.starts_with("trash (target in-root;"))
        .expect("the actual in-root route must be reported");
    assert!(
        report.contains(&t.join(".syncdash").join("trash").display().to_string()),
        "{report}"
    );
    assert!(
        !report.contains(&tr.display().to_string()),
        "the central path was not used: {report}"
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.starts_with("trash (central;")),
        "no central-trash report may be emitted when preservation stayed in-root: {messages:?}"
    );

    let kept = std::fs::read_dir(t.join(".syncdash").join("trash"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("f.txt"))
        .find(|path| path.exists())
        .expect("the reported in-root original must exist");
    assert_eq!(std::fs::read(kept).unwrap(), b"old");
    let _ = std::fs::remove_dir_all(&base);
}

/// The other half of the same decision: an ordinary local root keeps using the central store, so
/// the fix narrows the route rather than moving everyone onto the in-root area.
#[test]
fn an_on_machine_root_still_uses_the_central_trash_store() {
    let base = tmproot("onmachine");
    let (s, t, tr) = (base.join("s"), base.join("t"), base.join("trash"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("f.txt"), b"new").unwrap();
    std::fs::write(t.join("f.txt"), b"old").unwrap();

    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let copy = events.clone();
    let ctx = RunCtx::new(
        RunCtl::new(),
        Arc::new(move |event| copy.lock().unwrap().push(event)),
    );
    let out = apply::apply_with(
        &[op(Action::Update, "f.txt")],
        &s,
        &t,
        &opts(tr.clone()),
        &ctx,
    );
    assert_eq!((out.done, out.errors), (1, 0));
    assert_eq!(std::fs::read(tr.join("target/f.txt")).unwrap(), b"old");
    assert!(
        !t.join(".syncdash").exists(),
        "a local root needs no in-root retention area"
    );
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|event| matches!(event, ProgressEvent::Log {
        message,
        ..
    } if message.starts_with("trash (central;") && message.contains(&tr.display().to_string()))));
    let _ = std::fs::remove_dir_all(&base);
}

/// `VfsCaps::local_trash` describes reachability of the default store, not a custom path. An
/// external disk may be unable to reach that default while a user-selected trash directory on the
/// disk is a same-device rename target.
#[test]
fn a_custom_same_volume_trash_is_not_blocked_by_the_default_store_capability() {
    let base = tmproot("custom-same-volume-trash");
    let (s, t, tr) = (
        base.join("s"),
        base.join("external"),
        base.join("external-trash"),
    );
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(s.join("f.txt"), b"new").unwrap();
    std::fs::write(t.join("f.txt"), b"old").unwrap();

    let sv: Arc<dyn syncdash::fs::vfs::Vfs> =
        Arc::new(syncdash::fs::vfs::local::LocalVfs::open(s).unwrap());
    let tv: Arc<dyn syncdash::fs::vfs::Vfs> = Arc::new(RouteFixture(
        syncdash::fs::vfs::local::LocalVfs::open(t.clone()).unwrap(),
        Medium::RemovableDisk,
        true,
    ));
    assert!(
        !tv.caps().local_trash,
        "the fixture models an external root whose default store is unreachable"
    );

    let out = apply::apply_vfs(
        &[op(Action::Update, "f.txt")],
        &sv,
        &tv,
        &opts(tr.clone()),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (1, 0));
    assert_eq!(std::fs::read(t.join("f.txt")).unwrap(), b"new");
    assert_eq!(std::fs::read(tr.join("target/f.txt")).unwrap(), b"old");
    assert!(
        !t.join(".syncdash").exists(),
        "same-volume custom trash should not use in-root retention"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn copy_refuses_source_content_that_no_longer_matches_the_plan() {
    let base = tmproot("copy-source-content-drift");
    let (source, target) = (base.join("source"), base.join("target"));
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(source.join("file.bin"), b"newer-content").unwrap();

    let mut planned = op(Action::Copy, "file.bin");
    planned.size = Some(b"older-content".len() as u64);
    planned.hash = Some(blake3::hash(b"older-content").to_hex().to_string());
    let (done, _, errors) = apply::apply(&[planned], &source, &target, &opts(base.join("trash")));

    assert_eq!((done, errors), (0, 1));
    assert!(!target.join("file.bin").exists());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn copy_refuses_source_length_that_no_longer_matches_the_plan() {
    let base = tmproot("copy-source-length-drift");
    let (source, target) = (base.join("source"), base.join("target"));
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(source.join("file.bin"), b"changed length").unwrap();

    let mut planned = op(Action::Copy, "file.bin");
    planned.size = Some(3);
    let (done, _, errors) = apply::apply(&[planned], &source, &target, &opts(base.join("trash")));

    assert_eq!((done, errors), (0, 1));
    assert!(!target.join("file.bin").exists());
    let _ = std::fs::remove_dir_all(base);
}

/// The copy lane's width is a negotiation, not a preference.
///
/// A backend that declares `max_parallel_streams: 1` means it: FTP carries one control connection,
/// so a second concurrent transfer does not queue behind the first, it fails with "Data connection
/// is already open" and that file never lands. Verified against a live server — before the clamp a
/// three-file mirror over `ftp://` reported "1 done, 2 error(s)".
#[test]
fn the_copy_lane_never_exceeds_what_a_backend_declares() {
    use syncdash::fs::vfs::{Vfs, VfsCaps};

    // Borrow a real backend's sheet and vary only the one field under test, so the case cannot
    // drift out of shape when `VfsCaps` gains a member.
    let base = syncdash::fs::vfs::local::LocalVfs::open(std::env::temp_dir())
        .unwrap()
        .caps();
    let caps = |n: usize| VfsCaps {
        max_parallel_streams: n,
        ..base.clone()
    };
    let w = |pref, s, t| apply::copy_width(pref, &caps(s), &caps(t), false);

    assert_eq!(w(4, 1, 4), 1, "a single-stream source governs the pair");
    assert_eq!(w(4, 4, 1), 1, "and so does a single-stream target");
    assert_eq!(
        w(4, 16, 16),
        4,
        "a generous backend leaves the job's preference alone"
    );
    assert_eq!(
        w(8, 4, 4),
        4,
        "asking for more than the backends allow is still clamped"
    );
    assert_eq!(
        w(1, 16, 16),
        1,
        "asking for less is honoured — the clamp is a ceiling, not a floor"
    );
    assert_eq!(
        w(0, 16, 16),
        1,
        "width is never zero, or the lane would do nothing at all"
    );
    assert_eq!(
        apply::copy_width(4, &caps(16), &caps(16), true),
        1,
        "a duplicate (side, path) still forces sequential: two workers would race on one write"
    );
}
