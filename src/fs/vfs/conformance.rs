//! The backend contract as executable checks. Every `Vfs` implementation runs this
//! same suite; passing it is the admission ticket. Seeding happens *through the write
//! API itself*, so the suite needs nothing but a factory producing fresh empty roots.

use std::io::Read;
use std::sync::Arc;

use crate::model::table::EntryKind;
use super::error::VfsErrorKind;
use super::{Support, Vfs, WriteHint};
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
    use crate::fs::vfs::local::LocalVfs;
    use crate::fs::vfs::memory::MemVfs;

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
    fn memory_backend_conforms() {
        let mut n = 0;
        run_all(&mut || {
            n += 1;
            Arc::new(MemVfs::new(&format!("conformance-{n}"))) as Arc<dyn Vfs>
        });
    }

    /// The same contract against a **live SMB share**, for when the backend under it changes.
    ///
    /// Ignored by default because it needs a server: point `SYNCDASH_SMB_ROOT` at a writable
    /// directory on one and run
    ///
    /// ```text
    /// cargo test --lib os_route_smb_conforms -- --ignored --nocapture
    /// ```
    ///
    /// It exists to make the native-SMB question answerable: the same twelve checks that pass on
    /// a local disk have to pass through a real server before any backend claiming to speak SMB
    /// can replace the one that delegates to the OS. Running it against the *current* backend
    /// first is the point — it says what "passing" looks like today.
    #[test]
    #[ignore = "needs a live SMB share in SYNCDASH_SMB_ROOT"]
    fn os_route_smb_conforms() {
        let Ok(root) = std::env::var("SYNCDASH_SMB_ROOT") else {
            panic!("set SYNCDASH_SMB_ROOT to a writable directory on an SMB share");
        };
        let base = std::path::PathBuf::from(&root);
        assert!(base.is_dir(), "SYNCDASH_SMB_ROOT '{root}' is not a reachable directory");
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        let mut n = 0usize;
        run_all(&mut || {
            n += 1;
            let d = base.join(format!("conf-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            dirs.push(d.clone());
            Arc::new(LocalVfs::new(d)) as Arc<dyn Vfs>
        });
        for d in dirs {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// The same contract against a live share through the **native SMB2 backend** — the
    /// in-process client, no OS mount involved.
    ///
    /// This is the twin of `os_route_smb_conforms` above, and the pair is the point: that
    /// one measures what the OS-delegating backend does today, this one has to match it before
    /// the native client can be trusted with anyone's files. `set_mtime` in particular is the
    /// reason the backend exists in this order — it is the one `Vfs` method the SMB crate has
    /// no high-level call for, and `fs::lock::RootLock`'s heartbeat is a repeated `set_mtime`,
    /// so a backend that fails this check cannot hold a root lock.
    ///
    /// Needs a server *and* a stored credential — a native smb root cannot ride this machine's
    /// session login the way a `\\host\share` path can:
    ///
    /// ```text
    /// syncdash cred set "smb://<user>@<host>/<share>"
    /// set SYNCDASH_SMB_URL=smb://<user>@<host>/<share>/<scratch-dir>
    /// cargo test --lib smb_backend_conforms -- --ignored --nocapture
    /// ```
    ///
    /// Everything it makes lives under `SYNCDASH_SMB_URL` and is removed again on the way out.
    #[test]
    #[ignore = "needs a live SMB share in SYNCDASH_SMB_URL and a stored credential"]
    fn smb_backend_conforms() {
        use crate::fs::vfs::smb::SmbBackend;
        use crate::fs::vfs::spec::{parse, RootSpec};

        let Ok(base_url) = std::env::var("SYNCDASH_SMB_URL") else {
            panic!("set SYNCDASH_SMB_URL to an smb://user@host/share/subdir phrase");
        };
        let creds = crate::fs::vfs::cred::default_provider();
        let open = |url: &str| -> Arc<dyn Vfs> {
            let RootSpec::Remote(spec) = parse(url) else {
                panic!("'{url}' is not an smb:// phrase");
            };
            let b = SmbBackend::new(spec, creds.clone())
                .unwrap_or_else(|e| panic!("building a backend for '{url}': {e}"));
            b.connect().unwrap_or_else(|e| panic!("connecting to '{url}': {e}"));
            Arc::new(b) as Arc<dyn Vfs>
        };

        let base = open(&base_url);
        let mut roots: Vec<String> = Vec::new();
        let mut n = 0usize;
        run_all(&mut || {
            n += 1;
            let name = format!("conf-{}-{n}", std::process::id());
            // "Fresh and empty" has to hold even if an earlier run died mid-suite.
            remove_tree(&base, &name).unwrap_or_else(|e| panic!("clearing '{name}': {e}"));
            base.mkdir_all(&name).unwrap_or_else(|e| panic!("creating '{name}': {e}"));
            roots.push(name.clone());
            open(&format!("{base_url}/{name}"))
        });
        for name in roots {
            remove_tree(&base, &name).unwrap_or_else(|e| panic!("cleaning up '{name}': {e}"));
        }
    }

    /// Depth-first delete through the `Vfs` surface itself. The suite writes onto someone's
    /// real share, so it leaves it exactly as it found it; a name that is already gone is a
    /// success, not an error.
    #[cfg(test)]
    fn remove_tree(v: &Arc<dyn Vfs>, rel: &str) -> super::super::error::VfsResult<()> {
        let Some(m) = v.stat(rel)? else {
            return Ok(());
        };
        if m.kind == EntryKind::Dir {
            for (name, _) in v.read_dir_names(rel)? {
                remove_tree(v, &format!("{rel}/{name}"))?;
            }
            v.remove_dir(rel)
        } else {
            v.remove_file(rel)
        }
    }

    /// The SFTP capability shape: rename refuses to overwrite, mtimes land on a second boundary.
    #[test]
    fn sftp_shaped_caps_conform() {
        let mut n = 0;
        run_all(&mut || {
            n += 1;
            Arc::new(MemVfs::new(&format!("sftpish-{n}")).without(|c| {
                c.rename_overwrite = Support::No;
                c.mtime_precision_ms = 1000;
            })) as Arc<dyn Vfs>
        });
    }

    /// The LIST-only FTP shape: no ranged reads, no mtime setting, no symlinks, no read-back,
    /// no unix modes, minute-granular mtimes. Every optional check must skip rather than fail.
    #[test]
    fn ftp_shaped_caps_conform() {
        let mut n = 0;
        run_all(&mut || {
            n += 1;
            Arc::new(MemVfs::new(&format!("ftpish-{n}")).without(|c| {
                c.ranged_read = Support::No;
                c.set_mtime = Support::No;
                c.symlink = Support::No;
                c.read_back = Support::No;
                c.fsync = Support::No;
                c.unix_mode = Support::No;
                c.mtime_precision_ms = 60_000;
            })) as Arc<dyn Vfs>
        });
    }
}
