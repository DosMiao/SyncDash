//! The local lane: platform metadata traversal + rayon hashing over a real directory.
//!
//! macOS batches directory metadata with `getattrlistbulk`; other platforms use WalkDir. Both
//! feed the same records into this scanner, so filtering, cache use, hashing, progress, and error
//! policy remain one implementation. Hash parallelism is per *file*, not within one: reads use an
//! explicit loop rather than mapping because a vanished mapping kills the process with SIGBUS.

use std::collections::HashMap;
use std::path::Path;

use crate::foundation::time::now_ms;
use crate::model::table::{os_name, Entry, EntryKind, Header, Snapshot, SCHEMA};

use super::digest::{effective_read, sampled_digest_with_buffer, SAMPLE_MIN};
use super::local_walk::WalkKind;
use super::{ProgressFn, ScanMetrics, ScanOptions, ScanProgress};

#[cfg(not(target_os = "macos"))]
use super::local_walk::walk as walk_local;
#[cfg(target_os = "macos")]
use super::macos_bulk::walk as walk_local;

/// Read granularity, and therefore how often cancel/pause is honored mid-file.
const READ_CHUNK: u64 = 8 * 1024 * 1024;
const PROGRESS_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

fn progress_sample_due(stop: &std::sync::mpsc::Receiver<()>) -> bool {
    matches!(
        stop.recv_timeout(PROGRESS_SAMPLE_INTERVAL),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    )
}

fn full_hash_with_buffer(
    path: &Path,
    size: u64,
    buf: &mut Vec<u8>,
    mut checkpoint: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<String> {
    use std::io::Read;

    let mut f = std::fs::File::open(path)?;
    // Grow once to the largest file this worker has seen. Smaller files reuse the allocation and
    // read through only the prefix they need; a tree of tiny files no longer allocates per entry.
    let len = size.clamp(1, READ_CHUNK) as usize;
    if buf.len() < len {
        buf.resize(len, 0);
    }
    let mut hasher = blake3::Hasher::new();
    loop {
        checkpoint()?;
        let n = f.read(&mut buf[..len])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// The *other* iCloud eviction shape, and the dangerous one. When a file is evicted the real name
/// can disappear entirely and be replaced by a `.<name>.icloud` sidecar — a few hundred bytes of
/// plist. Nothing about that entry says "placeholder": it is a real file with a real size, so the
/// snapshot records the stub and records the original as *absent*. Against a backup that still
/// holds the original, mirror then plans a copy of the stub and a delete of the real file.
///
/// Matched on the name alone. Looking for the missing sibling would be the obvious test and it
/// cannot be done here: the walk is streaming, so the directory's full name set does not exist yet
/// at the moment this entry is judged.
fn is_icloud_stub(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".icloud") && name.len() > ".icloud".len() + 1
}

pub(super) fn scan_impl(
    root: &Path,
    opt: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
    ctxp: Option<(&crate::obs::progress::RunCtx, crate::model::event::Phase)>,
    max_parallel_streams: usize,
) -> std::io::Result<Snapshot> {
    let pp = ctxp.map(|(ctx, phase)| crate::obs::progress::PhaseProgress::begin(ctx, phase, Some(root.to_string_lossy().into_owned()), 0, 0));
    let side = match ctxp.map(|(_, ph)| ph) {
        Some(crate::model::event::Phase::ScanTarget) => "target",
        _ => "source",
    };
    let started = now_ms();
    let t0 = std::time::Instant::now();
    let mut metrics = ScanMetrics::default();
    let local_state = crate::store::localid::LocalScanStateIdentity::for_root(root);
    let measured = std::time::Instant::now();
    let cache = if opt.hash && opt.use_cache {
        crate::store::hashcache::load_local(&local_state)
    } else {
        HashMap::new()
    };
    metrics.cache_load_ms = measured.elapsed().as_millis() as u64;
    let measured = std::time::Instant::now();
    let mtime_fixes = crate::store::mtimefix::load_local(&local_state);
    metrics.mtime_load_ms = measured.elapsed().as_millis() as u64;
    let mut entries: Vec<Entry> = Vec::new();
    let mut hash_errors = 0u64;

    // Windows long paths: walk from a \\?\-prefixed root so every descendant path is immune to the 260-char MAX_PATH
    // (the OneDrive root prefix alone is 47 chars; deep course directories were measured hitting the limit, and a directory
    // that hits it vanishes silently along with its whole tree — in mirror mode that reads as "the other side got mass-deleted".
    // \\?\ is exactly what keeps FFS alive). The cache key and snapshot header keep the original root; the prefix lives only in the walk and in file I/O.
    //
    // **The prefix must not be pasted on by hand.** `\\?\` turns off Win32 path parsing, which means
    // '/' stops being a separator and '.' / '..' stop resolving — so the string that names a healthy
    // directory stops naming anything. Measured with the release binary on one 5-file tree: spelled
    // with backslashes it scanned 5 entries; spelled with forward slashes, or with a `..` in it, the
    // very same directory scanned **0**, because the root read failed and the loop simply counted a
    // walk error. A 0-entry source snapshot is not a degraded result — under mirror it means delete
    // everything on the far side. `canonicalize` returns an already-verbatim path (and the
    // `\\?\UNC\server\share` form for shares, which the hand-rolled version could not produce), so it
    // gives the same MAX_PATH immunity without inventing an unreadable spelling. If it fails we walk
    // the root as given: std applies the long-path prefix internally anyway.
    #[cfg(windows)]
    let walk_root: std::path::PathBuf = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    #[cfg(not(windows))]
    let walk_root: std::path::PathBuf = root.to_path_buf();

    // iCloud eviction, counted separately from walk errors because the remedy is different:
    // `brctl download` or an exclusion, not a permission.
    let mut icloud_stubs = 0u64;
    let mut icloud_stub_samples: Vec<String> = Vec::new();
    let mut dataless_files = 0u64;
    let mut skipped_symlinks = 0u64;

    // Phase 1: serial walk to collect entries (metadata is fast); phase 2: parallel hashing with rayon (the lesson from FFS
    // parallel_scan: with many small files the bottleneck is serial I/O). Every file is read through the same chunked loop, so the parallelism is across files and the buffer is sized to the file.
    struct PendingFile {
        rel: String,
        abs: std::path::PathBuf,
        size: u64,
        mt: i64,
        hash: Option<String>, // cache hit
        /// Set when hashing was attempted and failed: distinct from `hash: None`, which also covers
        /// "hashing was never requested". Only the first is a degraded judgment.
        hash_failed: bool,
        file_id: Option<String>,
        mode: Option<u32>,
    }
    let mut pending: Vec<PendingFile> = Vec::new();

    let measured = std::time::Instant::now();
    let walk_stats = walk_local(
        &walk_root,
        &opt.filter,
        || match &pp {
            Some(pp) => pp.checkpoint(),
            None => Ok(()),
        },
        |item| match item.kind {
            WalkKind::Dir => {
                let rel = item.rel;
                if opt.filter.pass_dir(&rel).0 {
                    entries.push(Entry { path: rel, kind: EntryKind::Dir, size: 0, mtime_ms: item.mtime_ms, hash: None, hash_failed: false, file_id: None, mode: None, link: None, prev: None });
                }
            }
            WalkKind::Symlink => {
                if !opt.symlinks_direct {
                    skipped_symlinks += 1;
                }
                if opt.symlinks_direct {
                    let target = std::fs::read_link(&item.abs).ok().map(|t| t.to_string_lossy().into_owned());
                    entries.push(Entry { path: item.rel, kind: EntryKind::Symlink, size: 0, mtime_ms: item.mtime_ms, hash: None, hash_failed: false, file_id: None, mode: None, link: target, prev: None });
                }
            }
            WalkKind::File => {
                let rel = item.rel;
                // Both iCloud eviction shapes, judged before the entry is recorded. Neither is an
                // exclusion and neither can be fixed by one: the stub's danger comes from the *absence*
                // of the real name, and excluding the stub only removes the visible hint while leaving
                // the delete in place.
                if is_icloud_stub(crate::foundation::path::base_name(&rel)) {
                    icloud_stubs += 1;
                    if icloud_stub_samples.len() < 5 {
                        icloud_stub_samples.push(rel.clone());
                    }
                } else if item.dataless {
                    dataless_files += 1;
                }
                let size = item.size;
                // P1-4: the filesystem once stored a different value than the mtime we asked for (FAT's 2-second granularity
                // / SMB truncation); convert it back to what we meant so compare need not lean on a tolerance
                let raw_mt = item.mtime_ms;
                let mt = match mtime_fixes.get(&rel) {
                    Some((ondisk, intended)) if *ondisk == raw_mt => *intended,
                    _ => raw_mt,
                };
                let mut hash = None;
                if opt.hash && opt.use_cache {
                    if let Some((cs, cm, ch)) = cache.get(&rel) {
                        // Cached values are isolated per mode: a `~` prefix means sampled digest, which must never stand in for a full hash (or the reverse)
                        let want_sampled = opt.sampled && size >= SAMPLE_MIN;
                        if *cs == size && *cm == mt && ch.starts_with('~') == want_sampled {
                            hash = Some(ch.clone());
                        }
                    }
                }
                pending.push(PendingFile {
                    rel,
                    abs: item.abs,
                    size,
                    mt,
                    hash,
                    hash_failed: false,
                    // FAT/exFAT synthesize object IDs from allocation state. On the real exFAT
                    // corpus, 1,532 zero-byte files changed IDs between two untouched scans, so
                    // persisting those values creates thousands of imaginary moves.
                    file_id: if local_state.file_ids_stable() {
                        item.file_id
                    } else {
                        None
                    },
                    mode: item.mode,
                });
                if let Some(pp) = &pp {
                    pp.item_done(&pending.last().unwrap().rel);
                }
            }
        },
    )?;
    let walk_errors = walk_stats.walk_errors;
    let walk_err_samples = walk_stats.walk_err_samples;
    let excl_dirs = walk_stats.excluded_dirs;
    let excl_files = walk_stats.excluded_files;
    metrics.walk_ms = measured.elapsed().as_millis() as u64;
    metrics.files = pending.len() as u64;
    metrics.cache_hits = pending.iter().filter(|file| file.hash.is_some()).count() as u64;

    if walk_errors > 0 {
        crate::log_warn!("scan", "warning: {walk_errors} entr(ies) under {} skipped by walk errors — they will look ABSENT on this side! samples: {}", root.display(), walk_err_samples.join(" | "));
        if let Some(pp) = &pp {
            pp.error("", "walk", side, &format!("{walk_errors} entr(ies) skipped by walk errors (they will be treated as ABSENT on this side!) samples: {}", walk_err_samples.join(" | ")));
        }
    }

    // The totals are known the moment the walk ends (the P2-6 insight): items = file count; bytes = the part that really has to be read off disk and hashed.
    // The item counter shifts gears: during the walk it counts "how many were found", during hashing it restarts from zero counting "how many are done"
    // —— otherwise the UI would sit at N/N from the very start of hashing.
    let bytes_to_hash: u64 = if opt.hash {
        pending.iter().filter(|p| p.hash.is_none()).map(|p| effective_read(p.size, opt.sampled)).sum()
    } else {
        0
    };
    let legacy_progress_totals = opt
        .hash
        .then_some((pending.len() as u64, bytes_to_hash));
    metrics.read_bytes = bytes_to_hash;
    metrics.workers = if opt.hash {
        max_parallel_streams.max(1).min(rayon::current_num_threads())
    } else {
        0
    };
    if let Some(pp) = &pp {
        if opt.hash {
            pp.restart_items_with_totals(pending.len() as u64, bytes_to_hash);
        } else {
            pp.set_totals(pending.len() as u64, bytes_to_hash);
        }
    }

    let hash_err_count = std::sync::atomic::AtomicU64::new(0);
    let measured = std::time::Instant::now();
    if opt.hash {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicU64, Ordering};
        // Only cache misses are actually read; the progress bar is only accurate if it counts their bytes
        let bytes_total = bytes_to_hash;
        let files_total = pending.len() as u64;
        let files_done = AtomicU64::new(0);
        let bytes_done = AtomicU64::new(0);
        let global_width = rayon::current_num_threads();
        let width = max_parallel_streams.max(1).min(global_width);
        let scan_pool = if width < global_width {
            Some(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(width)
                    .thread_name(|index| format!("sd-scan-{index}"))
                    .start_handler(|_| crate::foundation::thread::lower_priority())
                    .build()
                    .map_err(|error| std::io::Error::other(format!("cannot build {width}-thread scan pool: {error}")))?,
            )
        } else {
            None
        };

        if let Some(cb) = progress {
            cb(ScanProgress { phase: "walk", files_done: 0, files_total, bytes_total, bytes_done: 0, mib_per_s: 0.0, complete: false });
        }

        // Progress sampling gets a thread of its own (same structure as syncthing's ProgressTicker):
        // the hash threads only do one relaxed add and carry no callback overhead at all
        std::thread::scope(|sc| {
            let progress_stop = progress.map(|cb| {
                let (stop_tx, stop_rx) = std::sync::mpsc::channel();
                let (fd, bd, t_start) = (&files_done, &bytes_done, std::time::Instant::now());
                sc.spawn(move || {
                    while progress_sample_due(&stop_rx) {
                        let done_files = fd.load(Ordering::Relaxed);
                        let done = bd.load(Ordering::Relaxed);
                        let secs = t_start.elapsed().as_secs_f64().max(0.001);
                        cb(ScanProgress { phase: "hash", files_done: done_files, files_total, bytes_total, bytes_done: done, mib_per_s: done as f64 / secs / (1024.0 * 1024.0), complete: false });
                    }
                });
                stop_tx
            });
            // Every file is read through the same chunked loop, whatever its size. Two reasons, and
            // the second is why the small-file mmap fast path is gone:
            //
            // Cancellation. An mmap'd hash finishes the whole file before it can be interrupted (a
            // cloud placeholder would have to hydrate entirely first, possibly minutes). A
            // checkpoint every READ_CHUNK gives up blake3's intra-file multicore, but file-level
            // parallelism remains and the disk is almost always the bottleneck.
            //
            // Durability. Reading a mapped page whose backing file was truncated, or whose volume
            // disappeared, raises **SIGBUS** — a signal, not an io::Error — so the Err arm below
            // was unreachable and the process simply died: no dialog, no run-log summary, both root
            // locks left on disk because Drop never runs. On macOS a mounted SMB/AFP share under
            // /Volumes is an ordinary path and takes this lane, so a Wi-Fi roam mid-scan was enough.
            // The answer is not a SIGBUS handler in a process that writes user files; it is to not
            // map the file, so a vanishing file is an ordinary Err the hash-error path already
            // knows how to record.
            let mut hash_all = || {
                pending.par_iter_mut().for_each_init(Vec::new, |buf, p| {
                    if let Some(pp) = &pp {
                        if pp.checkpoint().is_err() {
                            return; // cancelled: the remaining work items spin out empty (the standard rayon early-exit)
                        }
                    }
                    if p.hash.is_none() {
                        // fast rigor tier: large files only read the three sample windows (a cloud placeholder hydrates only those three too)
                        if opt.sampled && p.size >= SAMPLE_MIN {
                            match sampled_digest_with_buffer(&p.abs, p.size, buf) {
                                Ok(d) => p.hash = Some(d),
                                Err(e) => {
                                    p.hash_failed = true;
                                    hash_err_count.fetch_add(1, Ordering::Relaxed);
                                    if let Some(pp) = &pp {
                                        pp.error(&p.rel, "hash", side, &e.to_string());
                                    }
                                }
                            }
                            let eff = effective_read(p.size, true);
                            bytes_done.fetch_add(eff, Ordering::Relaxed);
                            if let Some(pp) = &pp {
                                pp.add_bytes(eff, &p.rel);
                                pp.item_done(&p.rel);
                            }
                            files_done.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                        let res = full_hash_with_buffer(&p.abs, p.size, buf, || match &pp {
                            Some(pp) => pp.checkpoint(),
                            None => Ok(()),
                        });
                        match res {
                            Ok(hash) => p.hash = Some(hash),
                            Err(e) if crate::obs::progress::is_cancelled(&e) => return, // cancellation is not a hash error
                            Err(e) => {
                                p.hash_failed = true;
                                hash_err_count.fetch_add(1, Ordering::Relaxed);
                                if let Some(pp) = &pp {
                                    pp.error(&p.rel, "hash", side, &e.to_string());
                                }
                            }
                        }
                        bytes_done.fetch_add(p.size, Ordering::Relaxed);
                        if let Some(pp) = &pp {
                            pp.add_bytes(p.size, &p.rel);
                        }
                    }
                    if let Some(pp) = &pp {
                        pp.item_done(&p.rel); // item count during hashing = files processed (a cache hit bumps it immediately)
                    }
                    files_done.fetch_add(1, Ordering::Relaxed);
                })
            };
            if let Some(pool) = &scan_pool {
                pool.install(hash_all);
            } else {
                hash_all();
            }
            if let Some(stop) = progress_stop {
                let _ = stop.send(());
            }
        });
        if let Some(pp) = &pp {
            pp.checkpoint()?; // when the cancellation happened during hashing, this is where we honestly return Interrupted
        }

    }
    metrics.hash_ms = measured.elapsed().as_millis() as u64;
    let measured = std::time::Instant::now();
    for p in pending {
        entries.push(Entry { path: p.rel, kind: EntryKind::File, size: p.size, mtime_ms: p.mt, hash: p.hash, hash_failed: p.hash_failed, file_id: p.file_id, mode: p.mode, link: None, prev: None });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    metrics.finalize_ms = measured.elapsed().as_millis() as u64;
    let measured = std::time::Instant::now();
    if opt.hash {
        crate::store::hashcache::save_local(&local_state, &entries);
    }
    if walk_errors == 0 {
        crate::store::mtimefix::prune_local(&local_state, &mtime_fixes, &entries);
    }
    metrics.state_write_ms = measured.elapsed().as_millis() as u64;
    hash_errors += hash_err_count.load(std::sync::atomic::Ordering::Relaxed);
    if hash_errors > 0 {
        crate::log_warn!("scan", "warning: {hash_errors} file(s) could not be hashed (in use / unreadable)");
    }

    let snapshot = Snapshot {
        header: Header {
            schema: SCHEMA,
            kind: "snapshot".into(),
            root: root.to_string_lossy().into_owned(),
            host: crate::model::table::host_name(),
            os: os_name(),
            scanned_at_ms: started,
            duration_ms: t0.elapsed().as_millis() as u64,
            entry_count: entries.len() as u64,
            hashed: opt.hash,
            excluded_dirs: excl_dirs,
            excluded_files: excl_files,
            walk_errors,
            walk_err_samples,
            icloud_stubs,
            icloud_stub_samples,
            dataless_files,
            skipped_symlinks,
            vfs: None,
        },
        entries,
    };
    if let Some((ctx, _)) = ctxp {
        metrics.emit(ctx, side, "local");
    }
    if let Some(pp) = pp {
        pp.finish()?;
    }
    if let (Some(cb), Some((files_total, bytes_total))) = (progress, legacy_progress_totals) {
        cb(ScanProgress { phase: "hash", files_done: files_total, files_total, bytes_total, bytes_done: bytes_total, mib_per_s: 0.0, complete: true });
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::filter::PathFilter;
    use crate::pipeline::scan::{scan, scan_with_progress, ScanOptions};

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

        let mut buf = Vec::new();
        let first = full_hash_with_buffer(&large_path, large.len() as u64, &mut buf, || Ok(())).unwrap();
        assert_eq!(first, blake3::hash(&large).to_hex().to_string());
        let capacity = buf.capacity();

        let second = full_hash_with_buffer(&small_path, small.len() as u64, &mut buf, || Ok(())).unwrap();
        assert_eq!(second, blake3::hash(&small).to_hex().to_string());
        assert_eq!(buf.capacity(), capacity, "the next file must reuse rather than replace the worker allocation");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn progress_sampler_stop_is_wakeable() {
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        stop_tx.send(()).unwrap();
        assert!(!progress_sample_due(&stop_rx));
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
                state_ready_at_completion.store(
                    cache.len() == 8,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            events.lock().unwrap().push(progress);
        };
        scan_with_progress(&root, &opts(), Some(&collect)).unwrap();
        let events = events.into_inner().unwrap();
        assert!(events.len() >= 2);
        assert!(events[..events.len() - 1].iter().all(|progress| !progress.complete));
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
        assert!(bad.to_str().is_none(), "premise: the name is not valid Unicode");
        if std::fs::write(root.join(&bad), b"x").is_err() {
            let _ = std::fs::remove_dir_all(&root);
            return; // this filesystem refuses the name outright, which is also fine
        }

        let snap = scan(&root, &ScanOptions { hash: false, ..opts() }).unwrap();
        let paths: Vec<&str> = snap.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["good.txt"], "the unrepresentable name must not enter the table");
        assert!(!paths.iter().any(|p| p.contains('\u{FFFD}')), "a substituted spelling is worse than an omission: it points at a file that does not exist");

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
        let baseline = scan(&root, &ScanOptions { hash: false, ..opts() }).unwrap().entries.len();
        assert_eq!(baseline, 6, "5 files + 1 dir");

        let spellings = [base.replace('\\', "/"), format!("{base}{}sub{}..", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR), format!("{base}{}", std::path::MAIN_SEPARATOR), format!("{base}{}.", std::path::MAIN_SEPARATOR)];
        for s in spellings {
            let got = scan(Path::new(&s), &ScanOptions { hash: false, ..opts() }).unwrap_or_else(|e| panic!("scanning {s:?} failed: {e}"));
            assert_eq!(got.entries.len(), baseline, "{s:?} names the same directory and must produce the same table");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ...and when the root genuinely cannot be read, the answer is an error, never a snapshot
    /// that claims the tree is empty.
    #[test]
    fn an_unreadable_root_is_an_error_not_an_empty_table() {
        let missing = std::env::temp_dir().join(format!("syncdash-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        match scan(&missing, &ScanOptions { hash: false, ..opts() }) {
            Ok(s) => panic!("a missing root scanned as a {}-entry tree instead of erroring", s.entries.len()),
            Err(e) => assert!(e.to_string().contains("refusing to report it as an empty tree"), "{e}"),
        }
    }
}
