//! `pipeline::apply` against real directories.
//!
//! These were inline `#[cfg(test)]` tests, but every one of them builds a temp tree and drives
//! the public `apply` entry point — they are integration tests that happened to live inside the
//! module. Out here they keep `apply/mod.rs` to its actual production size, and they exercise the
//! crate the way a caller does.

use std::path::PathBuf;

use syncdash::fs::vfs::error::VfsResult;
use syncdash::fs::vfs::{
    Medium, ReadStream, VDirEntry, VMeta, VfsCaps, WriteHint, WriteStaged,
};
use syncdash::model::event::{ItemOutcome, ProgressEvent};
use syncdash::model::plan::{Action, Op, Side};
use syncdash::obs::progress::{RunCtl, RunCtx};
use syncdash::pipeline::apply::{self, ApplyOptions};
use std::sync::{Arc, Mutex};

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

fn opts(trash: PathBuf) -> ApplyOptions {
    ApplyOptions { dry_run: false, trash: Some(trash), fsync: false, ..Default::default() }
}

/// A ctx that collects ItemResult: the contents of the execution ledger (items.jsonl) are exactly these events
fn ledger_ctx() -> (RunCtx, std::sync::Arc<Mutex<Vec<(String, ItemOutcome, u64)>>>) {
    let store: std::sync::Arc<Mutex<Vec<(String, ItemOutcome, u64)>>> =
        std::sync::Arc::new(Mutex::new(Vec::new()));
    let s2 = store.clone();
    let sink = move |ev: syncdash::model::event::ProgressEvent| {
        if let syncdash::model::event::ProgressEvent::ItemResult { path, outcome, bytes, .. } = ev {
            s2.lock().unwrap().push((path, outcome, bytes));
        }
    };
    (RunCtx::new(syncdash::obs::progress::RunCtl::new(), std::sync::Arc::new(sink)), store)
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
        op(Action::Copy, "a.txt"),          // → ok
        op(Action::DeleteDir, "d"),         // → kept (non-empty and its contents are not deletable)
        op(Action::Copy, "missing.txt"),    // → failed (the source file does not exist)
    ];
    let out = apply::apply_with(&ops, &s, &t, &opts(base.join("trash")), &ctx);
    assert_eq!((out.done, out.skipped, out.errors), (1, 1, 1));

    let rows = log.lock().unwrap();
    // Key invariant: every entry in the plan leaves a trace in the ledger — not one more, not one fewer
    assert_eq!(rows.len(), ops.len(), "every op must leave a trace: {rows:?}");
    let find = |p: &str| rows.iter().find(|(path, _, _)| path == p).map(|(_, o, _)| *o);
    assert_eq!(find("a.txt"), Some(ItemOutcome::Ok));
    assert_eq!(find("d"), Some(ItemOutcome::Kept), "keeping the directory is not an error, but it must be traceable");
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
    let ops: Vec<Op> = (0..6).map(|i| op(Action::Copy, &format!("f{i}.txt"))).collect();
    let out = apply::apply_with(&ops, &s, &t, &opts(base.join("trash")), &ctx);

    assert_eq!(out.done, 0);
    let rows = log.lock().unwrap();
    // Assert honestly: the checkpoint stopped things **before any work began**, so not a single op ran
    // and the ledger naturally holds no rows. `all()` is vacuously true on an empty set, which would
    // turn this into a false pass — the row count must be pinned explicitly, or a future missing emit in record would go unnoticed.
    assert_eq!(rows.len(), 0, "the ledger must be empty when the cancel lands before any op: {rows:?}");
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

    let (done, _, errors) = apply::apply(&[op(Action::Copy, "a.txt")], &s, &t, &opts(base.join("trash")));
    assert_eq!((done, errors), (1, 0));
    assert_eq!(std::fs::read(t.join("a.txt")).unwrap(), b"hello");
    let leftovers: Vec<_> = std::fs::read_dir(&t)
        .unwrap()
        .flatten()
        .filter(|e| syncdash::fs::staged::is_temp_name(&e.file_name().to_string_lossy()))
        .collect();
    assert!(leftovers.is_empty(), "no temp files may survive a successful apply");
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

    let (done, _, errors) = apply::apply(&[op(Action::Update, "keep.txt")], &s, &t, &opts(base.join("trash")));
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
    assert_eq!(std::fs::read(tr.join("f.txt")).unwrap(), b"old", "old version must be recoverable");
    let _ = std::fs::remove_dir_all(&base);
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
        &[op(Action::Delete, "gone.bin"), op(Action::Update, "upd.bin")],
        &s,
        &t,
        &opts(tr.clone()),
    );
    assert_eq!(errors, 0, "read-only originals must not fail the ops");
    assert_eq!(done, 2);
    assert!(!t.join("gone.bin").exists(), "the read-only file must really be gone");
    assert_eq!(std::fs::read(t.join("upd.bin")).unwrap(), b"new");
    // and both originals are still recoverable from the trash
    assert_eq!(std::fs::read(tr.join("gone.bin")).unwrap(), b"old");
    assert_eq!(std::fs::read(tr.join("upd.bin")).unwrap(), b"old");

    // The assertions above pass on unix without exercising anything: `rm` of a 0444 file succeeds,
    // so the retry never fires and "errors == 0" proves nothing about the code this test names.
    // What matters here is the end state — the mode must come through untouched. The retry used to
    // be unconditional, and on unix `set_readonly(false)` is `mode |= 0o222`, so a file that took
    // the retry path was left group- and world-writable for good.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(tr.join("gone.bin")).unwrap().permissions().mode() & 0o777;
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
    let (done, skipped, errors) = apply::apply(&[op(Action::DeleteDir, "d")], &s, &t, &opts(base.join("trash")));
    assert_eq!(done, 0, "the directory was not actually removed");
    assert_eq!(skipped, 1, "it must be reported, not silently counted as done");
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

    let (done, skipped, errors) = apply::apply(&[op(Action::DeleteDir, "d")], &s, &t, &opts(base.join("trash")));
    assert_eq!(done, 0, "the directory must not be removed over a leftover symlink");
    assert_eq!(skipped, 1, "it must be reported, exactly as a protected file is");
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
    o.filter = Some(syncdash::pipeline::filter::PathFilter::build_full(&[], &[], &["*/*.tmp".to_string()]));
    let (done, _, errors) = apply::apply(&[op(Action::DeleteDir, "d")], &s, &t, &o);
    assert_eq!((done, errors), (1, 0));
    assert!(!t.join("d").exists(), "deletable leftovers must not block the directory removal");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn delta_update_produces_identical_content() {
    let base = tmproot("delta");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    // Larger than DELTA_MIN_SIZE, with only a short stretch in the middle differing
    let mut old = vec![0u8; 6 * 1024 * 1024];
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
    assert_eq!(std::fs::read(t.join("big.bin")).unwrap(), new, "delta path must be byte-exact");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn delta_update_handles_shrinking_files() {
    let base = tmproot("delta2");
    let (s, t) = (base.join("s"), base.join("t"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    let old = vec![7u8; 8 * 1024 * 1024];
    let new = vec![7u8; 5 * 1024 * 1024];
    std::fs::write(s.join("shrink.bin"), &new).unwrap();
    std::fs::write(t.join("shrink.bin"), &old).unwrap();

    let mut o = opts(base.join("trash"));
    o.delta = true;
    let (done, _, errors) = apply::apply(&[op(Action::Update, "shrink.bin")], &s, &t, &o);
    assert_eq!((done, errors), (1, 0));
    assert_eq!(std::fs::read(t.join("shrink.bin")).unwrap().len(), new.len(), "tail must be truncated");
    let _ = std::fs::remove_dir_all(&base);
}

// v0.9 M1: parallelism / progress / cancellation

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
    // The result trees agree file by file
    for e in std::fs::read_dir(&t1).unwrap().flatten() {
        let name = e.file_name();
        let a = std::fs::read(e.path()).unwrap();
        let b = std::fs::read(t2.join(&name)).unwrap();
        assert_eq!(a, b, "file {:?} differs between seq and par runs", name);
    }
    assert_eq!(
        std::fs::read_dir(&t1).unwrap().count(),
        std::fs::read_dir(&t2).unwrap().count()
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
    assert_eq!(out.bytes_copied, expect_bytes, "byte ledger must equal the sum of successful copies");
    let evs = store.lock().unwrap();
    assert_eq!(
        evs.iter().filter(|e| matches!(e, ProgressEvent::Error { .. })).count(),
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
    std::fs::write(s.join("big.bin"), vec![3u8; 64 * 1024 * 1024]).unwrap();
    let mut o = op(Action::Copy, "big.bin");
    o.size = Some(64 * 1024 * 1024);

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
    assert!(!t.join("big.bin").exists(), "no partial file may reach the destination");
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
    let ops = vec![op(Action::DeleteDir, "a"), op(Action::DeleteDir, "a/b"), op(Action::DeleteDir, "a/b/c")];
    let (done, _, errors) = apply::apply(&ops, &s, &t, &opts(base.join("trash")));
    assert_eq!((done, errors), (3, 0), "all three directories must go");
    assert!(!t.join("a").exists());
    let _ = std::fs::remove_dir_all(&base);
}

/// A root with a real path that is **not on this machine** — the `\\nas\share` shape.
///
/// This exact combination is where the cross-volume trash bug lived: `as_local()` is `Some`, so
/// delta, mmap hashing and the version store all correctly apply, while the central trash store
/// sits on the other side of a network link. Only a wrapper can stand in for it; a real one needs
/// a real share, and `memory::MemVfs` cannot (it has no path at all, so it never took this route).
struct OffMachine(syncdash::fs::vfs::local::LocalVfs);

impl syncdash::fs::vfs::Vfs for OffMachine {
    fn caps(&self) -> VfsCaps {
        VfsCaps { medium: Medium::NetworkShare, local_trash: false, ..self.0.caps() }
    }
    fn display(&self) -> String {
        self.0.display()
    }
    fn identity(&self) -> String {
        self.0.identity()
    }
    fn as_local(&self) -> Option<&std::path::Path> {
        self.0.as_local()
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

/// The bug: preserving an original chose its route from `as_local()`, so a share took the central
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
        Arc::new(syncdash::fs::vfs::local::LocalVfs::new(s.clone()));
    let tv: Arc<dyn syncdash::fs::vfs::Vfs> =
        Arc::new(OffMachine(syncdash::fs::vfs::local::LocalVfs::new(t.clone())));
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
    assert!(!tr.join("f.txt").exists(), "the original must not be copied off the root");

    // …it was renamed into the root's own retention area instead, recoverable with any browser
    let kept = std::fs::read_dir(t.join(".syncdash").join("trash"))
        .expect("in-root retention area must exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("f.txt"))
        .find(|p| p.exists())
        .expect("the original must be kept under <root>/.syncdash/trash/<run>/");
    assert_eq!(std::fs::read(kept).unwrap(), b"old", "old version must still be recoverable");
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

    let (done, _, errors) = apply::apply(&[op(Action::Update, "f.txt")], &s, &t, &opts(tr.clone()));
    assert_eq!((done, errors), (1, 0));
    assert_eq!(std::fs::read(tr.join("f.txt")).unwrap(), b"old");
    assert!(!t.join(".syncdash").exists(), "a local root needs no in-root retention area");
    let _ = std::fs::remove_dir_all(&base);
}
