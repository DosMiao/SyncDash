//! The backend contract as executable checks. Every `Vfs` implementation — local,
//! fake, and each protocol backend to come — runs this same suite; passing it is
//! the admission ticket. Seeding happens *through the write API itself*, so the
//! suite needs nothing but a factory producing fresh empty roots.

use std::io::Read;
use std::sync::Arc;

use super::error::VfsErrorKind;
use super::{EntryKind, Support, Vfs, WriteHint};
use crate::foundation::names::TEMP_PREFIX;

/// Run the whole contract. `mk` must return a **fresh, empty** root each call.
pub fn run_all(mk: &mut dyn FnMut() -> Arc<dyn Vfs>) {
    root_stat(mk());
    missing_is_confirmed_absent(mk());
    single_level_listing(mk());
    write_commit_visibility(mk());
    abandoned_write_leaves_nothing(mk());
    staged_len_reconciles(mk());
    rename_matches_declared_semantics(mk());
    remove_dir_classifies_nonempty(mk());
    ranged_read_matches_stream(mk());
    set_mtime_roundtrips_within_precision(mk());
    symlink_is_not_followed(mk());
    staged_read_back_returns_written_bytes(mk());
}

fn write_file(v: &Arc<dyn Vfs>, rel: &str, content: &[u8], mtime_ms: Option<i64>) {
    if let Some(parent) = crate::foundation::path::parent(rel) {
        v.mkdir_all(parent).expect("mkdir_all");
    }
    let hint = WriteHint { size_hint: Some(content.len() as u64), mtime_ms, mode: None };
    let mut w = v.open_write(rel, &hint).expect("open_write");
    w.write(content).expect("write");
    w.seal(false).expect("seal");
    w.commit().expect("commit");
}

fn read_file(v: &Arc<dyn Vfs>, rel: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    v.open_read(rel).expect("open_read").read_to_end(&mut buf).expect("read_to_end");
    buf
}

fn root_stat(v: Arc<dyn Vfs>) {
    let m = v.stat("").expect("stat root").expect("root exists");
    assert_eq!(m.kind, EntryKind::Dir, "rel=\"\" must name the root directory");
}

fn missing_is_confirmed_absent(v: Arc<dyn Vfs>) {
    assert!(v.stat("definitely/not/here.txt").expect("stat must not error").is_none());
}

fn single_level_listing(v: Arc<dyn Vfs>) {
    write_file(&v, "a/b/c.txt", b"x", None);
    write_file(&v, "a/top.txt", b"y", None);
    let names: Vec<String> = v.read_dir("a").expect("read_dir").into_iter().map(|e| e.name).collect();
    assert!(names.contains(&"b".to_string()), "listing must include the child dir: {names:?}");
    assert!(names.contains(&"top.txt".to_string()));
    assert!(
        !names.iter().any(|n| n.contains('/')),
        "read_dir is one level only, names are single segments: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("c.txt")),
        "grandchildren must not appear: {names:?}"
    );
}

fn write_commit_visibility(v: Arc<dyn Vfs>) {
    v.mkdir_all("d").unwrap();
    let mut w = v.open_write("d/f.bin", &WriteHint::default()).unwrap();
    w.write(b"payload").unwrap();
    // Before commit the destination does not exist; any staging artifact that IS
    // visible must carry the temp prefix so the filter recognizes it.
    assert!(v.stat("d/f.bin").unwrap().is_none(), "destination must not appear before commit");
    for e in v.read_dir("d").unwrap() {
        assert!(
            e.name.starts_with(TEMP_PREFIX),
            "visible staging artifact without temp prefix: {}",
            e.name
        );
    }
    w.seal(false).unwrap();
    w.commit().unwrap();
    let m = v.stat("d/f.bin").unwrap().expect("visible after commit");
    assert_eq!(m.kind, EntryKind::File);
    assert_eq!(m.size, 7);
    assert_eq!(read_file(&v, "d/f.bin"), b"payload");
}

fn abandoned_write_leaves_nothing(v: Arc<dyn Vfs>) {
    v.mkdir_all("d").unwrap();
    write_file(&v, "d/keep.txt", b"original", None);
    {
        let mut w = v.open_write("d/keep.txt", &WriteHint::default()).unwrap();
        w.write(b"half...").unwrap();
        // dropped without commit
    }
    assert_eq!(read_file(&v, "d/keep.txt"), b"original", "abandoned write must not touch the destination");
    for e in v.read_dir("d").unwrap() {
        assert!(!e.name.starts_with(TEMP_PREFIX), "abandoned temp not cleaned up: {}", e.name);
    }
}

fn staged_len_reconciles(v: Arc<dyn Vfs>) {
    let payload = vec![7u8; 130_000];
    let mut w = v.open_write("s.bin", &WriteHint::default()).unwrap();
    w.write(&payload[..100_000]).unwrap();
    w.write(&payload[100_000..]).unwrap();
    assert_eq!(w.staged_len().expect("staged_len"), 130_000);
    w.seal(false).unwrap();
    w.commit().unwrap();
}

fn rename_matches_declared_semantics(v: Arc<dyn Vfs>) {
    write_file(&v, "r/from.txt", b"mover", None);
    v.rename("r/from.txt", "r/to.txt").expect("plain rename");
    assert!(v.stat("r/from.txt").unwrap().is_none());
    assert_eq!(read_file(&v, "r/to.txt"), b"mover");

    write_file(&v, "r/blocker.txt", b"old", None);
    write_file(&v, "r/src.txt", b"new", None);
    match v.caps().rename_overwrite {
        Support::Yes => {
            v.rename("r/src.txt", "r/blocker.txt").expect("declared rename_overwrite=Yes");
            assert_eq!(read_file(&v, "r/blocker.txt"), b"new");
        }
        Support::No => {
            let e = v.rename("r/src.txt", "r/blocker.txt").expect_err("declared rename_overwrite=No");
            assert_eq!(e.kind, VfsErrorKind::AlreadyExists, "refusal must classify as AlreadyExists");
            assert_eq!(read_file(&v, "r/blocker.txt"), b"old", "refused rename must not damage the destination");
        }
        Support::Unknown => { /* nothing to pin down */ }
    }
}

fn remove_dir_classifies_nonempty(v: Arc<dyn Vfs>) {
    write_file(&v, "full/inner.txt", b"x", None);
    let e = v.remove_dir("full").expect_err("non-empty removal must fail");
    assert_eq!(e.kind, VfsErrorKind::NotEmpty, "the engine's delete-dir classification rides on this kind");
    v.remove_file("full/inner.txt").unwrap();
    v.remove_dir("full").expect("empty removal succeeds");
    assert!(v.stat("full").unwrap().is_none());
}

fn ranged_read_matches_stream(v: Arc<dyn Vfs>) {
    if !v.caps().ranged_read.yes() {
        return;
    }
    let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    write_file(&v, "big.bin", &payload, None);
    let full = read_file(&v, "big.bin");
    assert_eq!(full, payload);
    let mid = v.read_range("big.bin", 40_000, 4_096).expect("read_range");
    assert_eq!(&full[40_000..44_096], &mid[..]);
    let tail = v.read_range("big.bin", 99_900, 4_096).expect("read_range at eof");
    assert_eq!(tail.len(), 100, "short only at EOF");
    assert_eq!(&full[99_900..], &tail[..]);
}

fn set_mtime_roundtrips_within_precision(v: Arc<dyn Vfs>) {
    if !v.caps().set_mtime.yes() {
        return;
    }
    write_file(&v, "t.txt", b"x", None);
    let want: i64 = 1_600_000_123_456;
    v.set_mtime("t.txt", want).expect("set_mtime");
    let got = v.stat("t.txt").unwrap().unwrap().mtime_ms;
    let precision = v.caps().mtime_precision_ms.max(1) as i64;
    assert!(
        (got - want).abs() < precision + 1000,
        "mtime came back {got}, wanted {want} ± precision {precision}"
    );
}

fn symlink_is_not_followed(v: Arc<dyn Vfs>) {
    if !v.caps().symlink.yes() {
        return;
    }
    write_file(&v, "target.txt", b"real content", None);
    v.make_symlink("link.txt", "target.txt").expect("make_symlink");
    let m = v.stat("link.txt").unwrap().expect("link exists");
    assert_eq!(m.kind, EntryKind::Symlink, "stat is lstat: the link itself, never the target");
    let t = v.read_link("link.txt").expect("read_link");
    assert!(t.contains("target.txt"), "read_link returned {t}");
}

fn staged_read_back_returns_written_bytes(v: Arc<dyn Vfs>) {
    if !v.caps().read_back.yes() {
        return;
    }
    let payload = vec![42u8; 50_000];
    let mut w = v.open_write("rb.bin", &WriteHint::default()).unwrap();
    w.write(&payload).unwrap();
    w.seal(false).unwrap();
    let mut got = Vec::new();
    w.open_staged_read().expect("open_staged_read").read_to_end(&mut got).unwrap();
    assert_eq!(got, payload, "read-back must reproduce the staged bytes exactly");
    w.commit().unwrap();
}

#[cfg(test)]
mod suite {
    use super::*;
    use crate::fs::vfs::fake::FakeVfs;
    use crate::fs::vfs::local::LocalVfs;

    #[test]
    fn local_backend_conforms() {
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        let mut n = 0usize;
        run_all(&mut || {
            n += 1;
            let d = std::env::temp_dir().join(format!("syncdash-vfs-conf-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            dirs.push(d.clone());
            Arc::new(LocalVfs::new(d)) as Arc<dyn Vfs>
        });
        for d in dirs {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn fake_backend_conforms() {
        // The registry maps one phrase to one shared tree, so each sub-test's fresh
        // empty root needs a fresh phrase
        let mut n = 0;
        run_all(&mut || {
            n += 1;
            Arc::new(FakeVfs::from_phrase(&format!("fake://conformance-{n}")).unwrap()) as Arc<dyn Vfs>
        });
    }

    #[test]
    fn fake_backend_conforms_with_sftp_shaped_knobs() {
        // rename refuses overwrite, second-granular mtimes — the SFTP profile
        let mut n = 0;
        run_all(&mut || {
            n += 1;
            Arc::new(
                FakeVfs::from_phrase(&format!("fake://sftpish-{n}?no_rename_overwrite&precision_ms=1000")).unwrap(),
            ) as Arc<dyn Vfs>
        });
    }

    #[test]
    fn fake_backend_conforms_with_ftp_shaped_knobs() {
        // no ranged reads, no mtime setting, no symlinks, no read-back, minute mtimes — the LIST-only FTP profile
        let mut n = 0;
        run_all(&mut || {
            n += 1;
            Arc::new(
                FakeVfs::from_phrase(&format!(
                    "fake://ftpish-{n}?no_ranged_read&no_set_mtime&no_symlink&no_read_back&no_fsync&no_unix_mode&precision_ms=60000"
                ))
                .unwrap(),
            ) as Arc<dyn Vfs>
        });
    }
}
