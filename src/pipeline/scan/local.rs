//! The local lane: walkdir + rayon over a real directory.
//!
//! Kept byte-for-byte on the fast path — this is what runs for the overwhelming majority of
//! scans, and the parallel hashing here is the reason a cold scan of a large tree is bearable.
//! Parallelism is per *file*, not within one: hashing reads through an explicit loop rather than
//! mapping the file, because a mapped page whose backing store goes away kills the process with
//! SIGBUS instead of returning an error (see the hashing section for the full reasoning).

use std::collections::HashMap;
use std::path::Path;

use crate::foundation::time::now_ms;
use crate::model::table::{os_name, Entry, EntryKind, Header, Snapshot, SCHEMA};

use super::digest::{effective_read, sampled_digest, SAMPLE_MIN};
use super::{ProgressFn, ScanOptions, ScanProgress};

#[cfg(unix)]
fn file_id(md: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("{}:{}", md.dev(), md.ino()))
}

#[cfg(not(unix))]
fn file_id(_md: &std::fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
fn unix_mode(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(md.mode() & 0o7777)
}

#[cfg(not(unix))]
fn unix_mode(_md: &std::fs::Metadata) -> Option<u32> {
    None
}

/// macOS marks a file whose contents have been evicted to iCloud but whose name, size and mtime
/// remain. Reading one hydrates it — a full-rigor scan of a photo library would pull every byte
/// back down. Not a correctness problem (the bytes that arrive are the right bytes), which is why
/// it is reported and not refused.
#[cfg(target_os = "macos")]
fn is_dataless(md: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000; // sys/stat.h: "file is dataless object"
    md.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn is_dataless(_md: &std::fs::Metadata) -> bool {
    false
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

fn mtime_ms(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn scan_impl(
    root: &Path,
    opt: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
    ctxp: Option<(&crate::obs::progress::RunCtx, crate::model::event::Phase)>,
) -> std::io::Result<Snapshot> {
    let pp = ctxp.map(|(ctx, phase)| {
        crate::obs::progress::PhaseProgress::begin(ctx, phase, Some(root.to_string_lossy().into_owned()), 0, 0)
    });
    let side = match ctxp.map(|(_, ph)| ph) {
        Some(crate::model::event::Phase::ScanTarget) => "target",
        _ => "source",
    };
    let started = now_ms();
    let t0 = std::time::Instant::now();
    let cache = if opt.hash { crate::store::hashcache::load(root) } else { HashMap::new() };
    let mtime_fixes = crate::store::mtimefix::load(root);
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
    let walk_root: std::path::PathBuf =
        std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    #[cfg(not(windows))]
    let walk_root: std::path::PathBuf = root.to_path_buf();

    // Walk-phase errors are never silent again: count them, sample them, and say so honestly at the end
    // (a skipped entry is equivalent to "absent on this side" during compare —— an invisible fault that can produce a catastrophic plan)
    let mut walk_errors = 0u64;
    let mut walk_err_samples: Vec<String> = Vec::new();
    // iCloud eviction, counted separately from walk errors because the remedy is different:
    // `brctl download` or an exclusion, not a permission.
    let mut icloud_stubs = 0u64;
    let mut icloud_stub_samples: Vec<String> = Vec::new();
    let mut dataless_files = 0u64;

    // Exclusions must be visible: pruned directories/files are counted into the snapshot header so the UI can state "this much never took part in the comparison".
    // (Lesson learned: the default exclusions silently swallowed .git while the UI still said "both sides identical ✓" —— never again)
    let excl_dirs = std::cell::Cell::new(0u64);
    let excl_files = std::cell::Cell::new(0u64);
    let walker = walkdir::WalkDir::new(&walk_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let rel = e
                .path()
                .strip_prefix(&walk_root)
                .unwrap_or(e.path())
                .to_string_lossy()
                .replace('\\', "/");
            if e.file_type().is_dir() {
                let (pass, child_might_match) = opt.filter.pass_dir(&rel);
                let keep = pass || child_might_match;
                if !keep {
                    excl_dirs.set(excl_dirs.get() + 1); // the whole subtree is pruned
                }
                keep
            } else {
                let keep = opt.filter.pass_file(&rel);
                if !keep {
                    excl_files.set(excl_files.get() + 1);
                }
                keep
            }
        });

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

    for item in walker {
        if let Some(pp) = &pp {
            pp.checkpoint()?; // cancel/pause cooperation point (one relaxed atomic read per iteration)
        }
        let item = match item {
            Ok(i) => i,
            // A failure at depth 0 is the root itself being unreadable, and it must never be
            // downgraded to "this root has no entries". That snapshot is structurally valid and
            // says the tree is empty, so mirror reads it as "delete everything on the far side" —
            // the warning line scrolls past and the plan is a catastrophe. This is the same stance
            // scan_vfs already takes for a directory-level failure: refuse to emit a table rather
            // than emit one whose missing half reads as deletions.
            Err(e) if e.depth() == 0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!(
                        "scan of '{}' could not read the root itself: {e} — refusing to report it as an empty tree (that reads as a mass deletion on the other side)",
                        root.display()
                    ),
                ));
            }
            // Past the root the same rule holds, and for the same reason. walkdir emits a
            // directory before it descends, so a directory it then fails to read is already in
            // the table — with zero children. Counting and continuing leaves a structurally
            // valid snapshot that says the subtree is empty, and mirror turns that into a delete
            // for every file the other side still has.
            //
            // NotFound is the one honest exception: an entry that vanished between being listed
            // and being read is a scan race, not an unreadable tree. Everything else — EPERM from
            // a TCC-gated directory (~/Desktop, ~/Documents, any external volume), an ACL, a
            // dropped mount — means a subtree exists that this scan cannot see, and a table that
            // omits it is a table that lies. A loop error carries no io_error at all and aborts
            // for the same reason.
            Err(e) => {
                let kind = e.io_error().map(|io| io.kind());
                if kind != Some(std::io::ErrorKind::NotFound) {
                    return Err(std::io::Error::new(
                        kind.unwrap_or(std::io::ErrorKind::Other),
                        format!(
                            "scan of '{}' aborted at '{}': {e} — refusing to emit a half table (its missing subtrees would read as deletions)",
                            root.display(),
                            // Not unwrap_or_default: an empty string reads as "the root", which is
                            // the one place this cannot have happened. Say that the path is unknown.
                            e.path().map(|p| p.display().to_string()).unwrap_or_else(|| "<path unavailable>".into()),
                        ),
                    ));
                }
                walk_errors += 1;
                if walk_err_samples.len() < 5 {
                    walk_err_samples.push(format!(
                        "{}: {e}",
                        e.path().map(|p| p.display().to_string()).unwrap_or_else(|| "<path unavailable>".into())
                    ));
                }
                continue;
            }
        };
        if item.depth() == 0 {
            continue;
        }
        let raw_rel = item.path().strip_prefix(&walk_root).unwrap_or(item.path());
        // A name that is not valid Unicode must not enter the table. `to_string_lossy` would
        // hand back U+FFFD in its place, and that lossy spelling is a different path: apply
        // would join it against the root, miss the real file, and (in mirror) the original
        // would read as "absent on this side" forever. Linux and Samba shares hand out such
        // names routinely — a tree carried over from a legacy encoding is the usual source.
        // Route it into the walk-error channel, which already says out loud what a skipped
        // entry costs, rather than inventing a spelling nobody can act on.
        let Some(rel) = raw_rel.to_str() else {
            walk_errors += 1;
            if walk_err_samples.len() < 5 {
                walk_err_samples.push(format!(
                    "{}: name is not valid Unicode on this platform — skipped rather than recorded under a substituted spelling",
                    raw_rel.to_string_lossy()
                ));
            }
            continue;
        };
        let rel = rel.replace('\\', "/");
        let md = match item.metadata() {
            Ok(m) => m,
            Err(e) => {
                walk_errors += 1;
                if walk_err_samples.len() < 5 {
                    walk_err_samples.push(format!("{rel}: {e}"));
                }
                continue;
            }
        };
        if item.file_type().is_dir() {
            if opt.filter.pass_dir(&rel).0 {
                entries.push(Entry { path: rel, kind: EntryKind::Dir, size: 0, mtime_ms: mtime_ms(&md), hash: None, hash_failed: false, file_id: None, mode: None, link: None, prev: None });
            }
        } else if item.file_type().is_symlink() {
            if opt.symlinks_direct {
                let target = std::fs::read_link(item.path())
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned());
                entries.push(Entry { path: rel, kind: EntryKind::Symlink, size: 0, mtime_ms: mtime_ms(&md), hash: None, hash_failed: false, file_id: None, mode: None, link: target, prev: None });
            }
        } else {
            // Both iCloud eviction shapes, judged before the entry is recorded. Neither is an
            // exclusion and neither can be fixed by one: the stub's danger comes from the *absence*
            // of the real name, and excluding the stub only removes the visible hint while leaving
            // the delete in place.
            if is_icloud_stub(crate::foundation::path::base_name(&rel)) {
                icloud_stubs += 1;
                if icloud_stub_samples.len() < 5 {
                    icloud_stub_samples.push(rel.clone());
                }
            } else if is_dataless(&md) {
                dataless_files += 1;
            }
            let size = md.len();
            // P1-4: the filesystem once stored a different value than the mtime we asked for (FAT's 2-second granularity
            // / SMB truncation); convert it back to what we meant so compare need not lean on a tolerance
            let raw_mt = mtime_ms(&md);
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
            pending.push(PendingFile { rel, abs: item.path().to_path_buf(), size, mt, hash, hash_failed: false, file_id: file_id(&md), mode: unix_mode(&md) });
            if let Some(pp) = &pp {
                pp.item_done(&pending.last().unwrap().rel);
            }
        }
    }

    if walk_errors > 0 {
        crate::log_warn!(
            "scan",
            "warning: {walk_errors} entr(ies) under {} skipped by walk errors — they will look ABSENT on this side! samples: {}",
            root.display(),
            walk_err_samples.join(" | ")
        );
        if let Some(pp) = &pp {
            pp.error(
                "",
                "walk",
                side,
                &format!("{walk_errors} entr(ies) skipped by walk errors (they will be treated as ABSENT on this side!) samples: {}", walk_err_samples.join(" | ")),
            );
        }
    }

    // The totals are known the moment the walk ends (the P2-6 insight): items = file count; bytes = the part that really has to be read off disk and hashed.
    // The item counter shifts gears: during the walk it counts "how many were found", during hashing it restarts from zero counting "how many are done"
    // —— otherwise the UI would sit at N/N from the very start of hashing.
    if let Some(pp) = &pp {
        let bytes_to_hash: u64 = if opt.hash {
            pending.iter().filter(|p| p.hash.is_none()).map(|p| effective_read(p.size, opt.sampled)).sum()
        } else {
            0
        };
        pp.set_totals(pending.len() as u64, bytes_to_hash);
        pp.restart_items();
    }

    let hash_err_count = std::sync::atomic::AtomicU64::new(0);
    if opt.hash {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        /// Read granularity, and therefore how often cancel/pause is honoured mid-file.
        const READ_CHUNK: u64 = 8 * 1024 * 1024;
        // Only cache misses are actually read; the progress bar is only accurate if it counts their bytes
        let bytes_total: u64 = pending.iter().filter(|p| p.hash.is_none()).map(|p| p.size).sum();
        let files_total = pending.len() as u64;
        let bytes_done = AtomicU64::new(0);
        let hashing = AtomicBool::new(true);

        if let Some(cb) = progress {
            cb(ScanProgress { phase: "walk", files_total, bytes_total, bytes_done: 0, mib_per_s: 0.0 });
        }

        // Progress sampling gets a thread of its own (same structure as syncthing's ProgressTicker):
        // the hash threads only do one relaxed add and carry no callback overhead at all
        std::thread::scope(|sc| {
            if let Some(cb) = progress {
                let (bd, hz, t_start) = (&bytes_done, &hashing, std::time::Instant::now());
                sc.spawn(move || {
                    while hz.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        let done = bd.load(Ordering::Relaxed);
                        let secs = t_start.elapsed().as_secs_f64().max(0.001);
                        cb(ScanProgress {
                            phase: "hash",
                            files_total,
                            bytes_total,
                            bytes_done: done,
                            mib_per_s: done as f64 / secs / (1024.0 * 1024.0),
                        });
                    }
                });
            }
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
            pending.par_iter_mut().for_each(|p| {
                if let Some(pp) = &pp {
                    if pp.checkpoint().is_err() {
                        return; // cancelled: the remaining work items spin out empty (the standard rayon early-exit)
                    }
                }
                if p.hash.is_none() {
                    // fast rigor tier: large files only read the three sample windows (a cloud placeholder hydrates only those three too)
                    if opt.sampled && p.size >= SAMPLE_MIN {
                        match sampled_digest(&p.abs, p.size) {
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
                        return;
                    }
                    let mut hasher = blake3::Hasher::new();
                    let res = (|| -> std::io::Result<()> {
                        use std::io::Read;
                        let mut f = std::fs::File::open(&p.abs)?;
                        // Sized to the file, capped at the checkpoint interval: a tree of small
                        // files must not allocate 8MiB per rayon worker.
                        let cap = p.size.clamp(1, READ_CHUNK) as usize;
                        let mut buf = vec![0u8; cap];
                        loop {
                            if let Some(pp) = &pp {
                                pp.checkpoint()?;
                            }
                            let n = f.read(&mut buf)?;
                            if n == 0 {
                                break;
                            }
                            hasher.update(&buf[..n]);
                        }
                        Ok(())
                    })();
                    match res {
                        Ok(_) => p.hash = Some(hasher.finalize().to_hex().to_string()),
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
            });
            hashing.store(false, Ordering::Relaxed);
        });
        if let Some(pp) = &pp {
            pp.checkpoint()?; // when the cancellation happened during hashing, this is where we honestly return Interrupted
        }

        if let Some(cb) = progress {
            cb(ScanProgress {
                phase: "hash",
                files_total,
                bytes_total,
                bytes_done: bytes_total,
                mib_per_s: 0.0,
            });
        }
    }
    for p in pending {
        entries.push(Entry { path: p.rel, kind: EntryKind::File, size: p.size, mtime_ms: p.mt, hash: p.hash, hash_failed: p.hash_failed, file_id: p.file_id, mode: p.mode, link: None, prev: None });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    if opt.hash {
        crate::store::hashcache::save_by_key(&root.to_string_lossy(), &entries);
    }
    hash_errors += hash_err_count.load(std::sync::atomic::Ordering::Relaxed);
    if hash_errors > 0 {
        crate::log_warn!("scan", "warning: {hash_errors} file(s) could not be hashed (in use / unreadable)");
    }

    Ok(Snapshot {
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
            excluded_dirs: excl_dirs.get(),
            excluded_files: excl_files.get(),
            walk_errors,
            walk_err_samples,
            icloud_stubs,
            icloud_stub_samples,
            dataless_files,
            vfs: None,
        },
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::filter::PathFilter;
    use crate::pipeline::scan::{scan, ScanOptions};

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
        let baseline = scan(&root, &ScanOptions { hash: false, ..opts() }).unwrap().entries.len();
        assert_eq!(baseline, 6, "5 files + 1 dir");

        let spellings = [
            base.replace('\\', "/"),
            format!("{base}{}sub{}..", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR),
            format!("{base}{}", std::path::MAIN_SEPARATOR),
            format!("{base}{}.", std::path::MAIN_SEPARATOR),
        ];
        for s in spellings {
            let got = scan(Path::new(&s), &ScanOptions { hash: false, ..opts() })
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
        match scan(&missing, &ScanOptions { hash: false, ..opts() }) {
            Ok(s) => panic!("a missing root scanned as a {}-entry tree instead of erroring", s.entries.len()),
            Err(e) => assert!(e.to_string().contains("refusing to report it as an empty tree"), "{e}"),
        }
    }
}
