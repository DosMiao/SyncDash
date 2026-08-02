use super::hashing::reader::{full_hash_with_buffer, standard_mtime_ms};
use super::progress::{hash_sample_due, walk_sample_due, WALK_INTERVAL};
use super::*;
use crate::foundation::path::RootRelativePath;
use crate::model::digest::Blake3Digest;
use crate::pipeline::filter::PathFilter;
use crate::pipeline::scan::{scan, scan_with_progress, ScanOptions};
use std::path::Path;

fn opts() -> ScanOptions {
    ScanOptions {
        hash: true,
        sampled: false,
        use_cache: false, // tests never eat the cache
        symlinks_direct: false,
        filter: PathFilter::build(&[], &[]),
    }
}

#[test]
fn full_hash_reuses_the_worker_buffer_across_files() {
    let root = std::env::temp_dir().join(format!("syncdash-hash-buffer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let large = vec![17u8; 64 * 1024];
    let small = vec![29u8; 17];
    let large_path = root.join("large.bin");
    let small_path = root.join("small.bin");
    std::fs::write(&large_path, &large).unwrap();
    std::fs::write(&small_path, &small).unwrap();
    let local_root = LocalRoot::open(root.clone()).unwrap();
    let large_relative = RootRelativePath::new("large.bin").unwrap();
    let small_relative = RootRelativePath::new("small.bin").unwrap();

    let mut buf = Vec::new();
    let first = full_hash_with_buffer(
        &local_root,
        &large_relative,
        large.len() as u64,
        standard_mtime_ms(&std::fs::metadata(&large_path).unwrap()),
        None,
        &mut buf,
        || Ok(()),
        |_| {},
    )
    .unwrap();
    assert_eq!(first, Blake3Digest::from_hash(blake3::hash(&large)));
    let capacity = buf.capacity();

    let second = full_hash_with_buffer(
        &local_root,
        &small_relative,
        small.len() as u64,
        standard_mtime_ms(&std::fs::metadata(&small_path).unwrap()),
        None,
        &mut buf,
        || Ok(()),
        |_| {},
    )
    .unwrap();
    assert_eq!(second, Blake3Digest::from_hash(blake3::hash(&small)));
    assert_eq!(
        buf.capacity(),
        capacity,
        "the next file must reuse rather than replace the worker allocation"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn progress_sampler_stop_is_wakeable() {
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    stop_tx.send(()).unwrap();
    assert!(!hash_sample_due(&stop_rx));
}

#[test]
fn walk_progress_cadence_is_immediate_then_throttled() {
    let mut last = None;
    assert!(walk_sample_due(&mut last, std::time::Duration::ZERO));
    assert!(!walk_sample_due(
        &mut last,
        WALK_INTERVAL - std::time::Duration::from_millis(1),
    ));
    assert!(walk_sample_due(&mut last, WALK_INTERVAL,));
}

#[test]
fn zero_byte_progress_has_one_explicit_completion_boundary() {
    let root = std::env::temp_dir().join(format!("syncdash-zero-progress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..8 {
        std::fs::write(root.join(format!("empty-{index}")), b"").unwrap();
    }

    let events = std::sync::Mutex::new(Vec::new());
    let state_ready_at_completion = std::sync::atomic::AtomicBool::new(false);
    let collect = |progress: ScanProgress| {
        if progress.complete {
            let state = crate::store::localid::LocalScanStateIdentity::for_root(&root);
            let cache = crate::store::hashcache::load_local(&state);
            state_ready_at_completion.store(cache.len() == 8, std::sync::atomic::Ordering::Relaxed);
        }
        events.lock().unwrap().push(progress);
    };
    scan_with_progress(&root, &opts(), Some(&collect)).unwrap();
    let events = events.into_inner().unwrap();
    assert!(events.len() >= 2);
    assert!(events[..events.len() - 1]
        .iter()
        .all(|progress| !progress.complete));
    let final_event = events.last().unwrap();
    assert!(final_event.complete);
    assert_eq!(final_event.files_done, 8);
    assert_eq!(final_event.files_total, 8);
    assert_eq!(final_event.bytes_done, 0);
    assert_eq!(final_event.bytes_total, 0);
    assert!(
        state_ready_at_completion.load(std::sync::atomic::Ordering::Relaxed),
        "the completion callback must run after the finished hash cache is visible",
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn no_hash_progress_still_has_one_terminal_completion() {
    let root =
        std::env::temp_dir().join(format!("syncdash-no-hash-progress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..3 {
        std::fs::write(root.join(format!("file-{index}")), b"contents").unwrap();
    }

    let events = std::sync::Mutex::new(Vec::new());
    let collect = |progress: ScanProgress| events.lock().unwrap().push(progress);
    let mut options = opts();
    options.hash = false;
    scan_with_progress(&root, &options, Some(&collect)).unwrap();

    let events = events.into_inner().unwrap();
    assert_eq!(events.iter().filter(|event| event.complete).count(), 1);
    assert!(events.iter().any(|event| {
        event.phase == "walk" && !event.complete && event.files_done > 0 && event.files_total == 0
    }));
    let terminal = events.last().unwrap();
    assert_eq!(terminal.phase, "walk");
    assert_eq!(terminal.files_done, 3);
    assert_eq!(terminal.files_total, 3);
    assert_eq!(terminal.bytes_done, 0);
    assert_eq!(terminal.bytes_total, 0);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn corrected_mtime_never_reuses_a_hash_for_a_replacement_object() {
    let root = std::env::temp_dir().join(format!(
        "syncdash-corrected-cache-replacement-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("same.bin");
    let raw_ms = 1_700_000_000_000i64;
    let intended_ms = raw_ms + 1_500;
    let stamp = filetime::FileTime::from_unix_time(raw_ms / 1_000, 0);
    std::fs::write(&file, b"old!").unwrap();
    filetime::set_file_mtime(&file, stamp).unwrap();

    let state = crate::store::localid::LocalScanStateIdentity::for_root(&root);
    assert_ne!(
        crate::store::mtimefix::record_local(&state, &[("same.bin".into(), raw_ms, intended_ms)],),
        crate::store::StateWriteStatus::Failed,
    );
    let mut options = opts();
    options.use_cache = true;
    let first = scan(&root, &options).unwrap();
    let first_hash = first.entries[0]
        .as_file()
        .and_then(|file| file.identity.digest())
        .cloned()
        .unwrap();
    assert_eq!(first.entries[0].mtime_ms(), intended_ms);

    std::fs::remove_file(&file).unwrap();
    std::fs::write(&file, b"new!").unwrap();
    filetime::set_file_mtime(&file, stamp).unwrap();
    let second = scan(&root, &options).unwrap();
    assert_eq!(second.entries[0].mtime_ms(), intended_ms);
    assert_ne!(
        second.entries[0]
            .as_file()
            .and_then(|file| file.identity.digest()),
        Some(&first_hash),
        "a same-size replacement with the same coarse raw mtime must be re-read",
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn filtered_no_cache_scan_retains_hashes_outside_its_domain() {
    let root = std::env::temp_dir().join(format!(
        "syncdash-filtered-no-cache-local-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("excluded")).unwrap();
    let included = root.join("included.bin");
    std::fs::write(&included, b"old!").unwrap();
    std::fs::write(root.join("excluded/keep.bin"), b"outside").unwrap();
    let stamp = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_mtime(&included, stamp).unwrap();

    let first = scan(&root, &opts()).unwrap();
    let old_hash = first
        .entries
        .iter()
        .find(|entry| entry.path().as_str() == "included.bin")
        .and_then(|entry| entry.as_file())
        .and_then(|file| file.identity.digest().cloned())
        .unwrap();

    // Same size and mtime would be a cache hit if loading state accidentally weakened the
    // no-cache tier. The filtered scan must re-read this file while retaining the other row.
    std::fs::write(&included, b"new!").unwrap();
    filetime::set_file_mtime(&included, stamp).unwrap();
    let mut filtered = opts();
    filtered.filter = PathFilter::build(&[], &["/excluded/".into()]);
    let second = scan(&root, &filtered).unwrap();
    let new_hash = second
        .entries
        .iter()
        .find(|entry| entry.path().as_str() == "included.bin")
        .and_then(|entry| entry.as_file())
        .and_then(|file| file.identity.digest())
        .unwrap();
    assert_ne!(new_hash, &old_hash);

    let state = crate::store::localid::LocalScanStateIdentity::for_root(&root);
    let cache = crate::store::hashcache::load_local(&state);
    assert!(cache.contains_key("excluded/keep.bin"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_name_that_is_not_valid_unicode_is_skipped_not_substituted() {
    let root = std::env::temp_dir().join(format!("syncdash-wtf8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("good.txt"), b"ok").unwrap();

    #[cfg(windows)]
    let bad: std::ffi::OsString = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0x0062, 0xD800, 0x0063]) // b<lone surrogate>c
    };
    #[cfg(unix)]
    let bad: std::ffi::OsString = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![b'b', 0xFF, b'c'])
    };
    assert!(
        bad.to_str().is_none(),
        "premise: the name is not valid Unicode"
    );
    if std::fs::write(root.join(&bad), b"x").is_err() {
        let _ = std::fs::remove_dir_all(&root);
        return; // this filesystem refuses the name outright, which is also fine
    }

    let snap = scan(
        &root,
        &ScanOptions {
            hash: false,
            ..opts()
        },
    )
    .unwrap();
    let paths: Vec<&str> = snap
        .entries
        .iter()
        .map(|entry| entry.path().as_str())
        .collect();
    assert_eq!(
        paths,
        vec!["good.txt"],
        "the unrepresentable name must not enter the table"
    );
    assert!(
        !paths.iter().any(|p| p.contains('\u{FFFD}')),
        "a substituted spelling is worse than an omission: it points at a file that does not exist"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The same directory must scan the same no matter how its root is spelled. It did not:
/// the hand-pasted `\\?\` prefix turned off Win32 path parsing, so a root written with
/// forward slashes or containing `..` named nothing, and the scan reported an empty tree —
/// which mirror reads as "delete everything on the far side".
#[test]
fn equivalent_root_spellings_scan_identically() {
    let root = std::env::temp_dir().join(format!("syncdash-spell-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    for i in 0..5 {
        std::fs::write(root.join(format!("f{i}.txt")), b"x").unwrap();
    }
    let base = root.to_string_lossy().into_owned();
    let baseline = scan(
        &root,
        &ScanOptions {
            hash: false,
            ..opts()
        },
    )
    .unwrap()
    .entries
    .len();
    assert_eq!(baseline, 6, "5 files + 1 dir");

    let spellings = [
        base.replace('\\', "/"),
        format!(
            "{base}{}sub{}..",
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        ),
        format!("{base}{}", std::path::MAIN_SEPARATOR),
        format!("{base}{}.", std::path::MAIN_SEPARATOR),
    ];
    for s in spellings {
        let got = scan(
            Path::new(&s),
            &ScanOptions {
                hash: false,
                ..opts()
            },
        )
        .unwrap_or_else(|e| panic!("scanning {s:?} failed: {e}"));
        assert_eq!(
            got.entries.len(),
            baseline,
            "{s:?} names the same directory and must produce the same table"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// ...and when the root genuinely cannot be read, the answer is an error, never a snapshot
/// that claims the tree is empty.
#[test]
fn an_unreadable_root_is_an_error_not_an_empty_table() {
    let missing = std::env::temp_dir().join(format!("syncdash-absent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&missing);
    match scan(
        &missing,
        &ScanOptions {
            hash: false,
            ..opts()
        },
    ) {
        Ok(s) => panic!(
            "a missing root scanned as a {}-entry tree instead of erroring",
            s.entries.len()
        ),
        Err(e) => assert!(
            e.to_string()
                .contains("refusing to report it as an empty tree"),
            "{e}"
        ),
    }
}
