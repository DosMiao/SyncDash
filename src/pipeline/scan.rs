//! scan: walk the directory tree and produce a snapshot table (the "generic command" of requirement 1).
//! - Excludes known junk / rebuildable directories (a subset matching CodeSync's FFS exclusion rules)
//! - blake3 content hash, with a cache: if (path,size,mtime) is unchanged, reuse the previous hash instead of rehashing tens of GB every run
//! - The cache lives in the local user cache directory and never pollutes the scanned tree

use crate::foundation::time::now_ms;
use crate::model::table::{os_name, Entry, EntryKind, Header, Snapshot, SCHEMA};
use std::collections::HashMap;
use std::path::Path;

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

fn mtime_ms(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct ScanOptions {
    pub hash: bool,
    /// Sampled evidence: files ≥4MB are not read whole but get a sampled digest (size + blake3 of 256KB at head/middle/tail,
    /// the value `~`-prefixed to keep it strictly apart from a full hash); <4MB is hashed in full. Cloud placeholders hydrate
    /// only those three windows. Not a byte-for-byte equality proof —— the escalation rule (same digest, different mtime → full rehash) backstops it.
    pub sampled: bool,
    /// Whether to trust the (path,size,mtime) cache. **The ladder's decisive axis**:
    /// fast = true (only the changed surface is really read; the unchanged surface is cache memory);
    /// standard/paranoid = false (every file is really read this run —— "identical ✓" is measured now, not remembered).
    pub use_cache: bool,
    /// symlinks="direct": record the link itself (its target string); otherwise symlinks are ignored
    pub symlinks_direct: bool,
    /// Filter with FFS semantics (see filter.rs); the default exclusions are built in
    pub filter: crate::pipeline::filter::PathFilter,
}

/// Parameters and implementation of the sampled digest (the fast rigor tier)
pub const SAMPLE_MIN: u64 = 4 * 1024 * 1024;
const SAMPLE_CHUNK: usize = 256 * 1024;

/// Bytes this scan will actually read in fast mode (only counting these makes the progress total and rate honest)
fn effective_read(size: u64, sampled: bool) -> u64 {
    if sampled && size >= SAMPLE_MIN {
        (3 * SAMPLE_CHUNK as u64).min(size)
    } else {
        size
    }
}

fn sampled_digest(path: &Path, size: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    let mut buf = vec![0u8; SAMPLE_CHUNK];
    for off in [0u64, size / 2, size.saturating_sub(SAMPLE_CHUNK as u64)] {
        f.seek(SeekFrom::Start(off))?;
        let mut read = 0usize;
        while read < SAMPLE_CHUNK {
            let n = f.read(&mut buf[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("~{}", hasher.finalize().to_hex()))
}

/// Scan progress (P2-6). The same amount of information as syncthing's `FolderScanProgress`:
/// phase + bytes done/total + rate, enough for the frontend to draw a bar and estimate the time remaining.
#[derive(Clone, Copy, Debug)]
pub struct ScanProgress {
    /// "walk" (metadata traversal) | "hash" (parallel hashing)
    pub phase: &'static str,
    pub files_total: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub mib_per_s: f64,
}

pub type ProgressFn<'a> = &'a (dyn Fn(ScanProgress) + Sync);

pub fn scan(root: &Path, opt: &ScanOptions) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, None, None)
}

pub fn scan_with_progress(
    root: &Path,
    opt: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, progress, None)
}

/// v0.9 M1 unified-foundation entry point: cancel/pause/ProgressEvent event stream (see progress.rs).
/// The old ScanProgress callback shape (P2-6) is kept as-is —— both paths share the same scan_impl.
pub fn scan_ctx(
    root: &Path,
    opt: &ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
    phase: crate::model::event::Phase,
) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, None, Some((ctx, phase)))
}

/// Route a root to the right scan lane: a local (or locally-translated) root keeps the
/// existing walkdir+mmap fast path byte-for-byte; everything else runs the generic VFS
/// lane. Both lanes speak the same filter contract and the same exclusion accounting —
/// the differential tests pin those numbers against each other.
pub fn scan_root(
    vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
    phase: crate::model::event::Phase,
) -> std::io::Result<Snapshot> {
    match vfs.as_local() {
        Some(root) => scan_impl(root, opt, None, Some((ctx, phase))),
        None => scan_vfs(vfs, opt, ctx, phase),
    }
}

fn sampled_digest_vfs(vfs: &dyn crate::fs::vfs::Vfs, rel: &str, size: u64) -> Result<String, crate::fs::vfs::error::VfsError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    for off in [0u64, size / 2, size.saturating_sub(SAMPLE_CHUNK as u64)] {
        let buf = vfs.read_range(rel, off, SAMPLE_CHUNK as u32)?;
        hasher.update(&buf);
    }
    // Same windows, same size prefix, same `~` marker as the local sampled_digest —
    // the two lanes must produce identical digests for identical content
    Ok(format!("~{}", hasher.finalize().to_hex()))
}

fn full_hash_vfs(
    vfs: &dyn crate::fs::vfs::Vfs,
    rel: &str,
    pp: &crate::obs::progress::PhaseProgress<'_>,
) -> Result<String, crate::fs::vfs::error::VfsError> {
    let mut stream = vfs.open_read(rel)?;
    let mut hasher = blake3::Hasher::new();
    let block = stream.block_size().clamp(64 * 1024, 8 * 1024 * 1024);
    let mut buf = vec![0u8; block];
    loop {
        pp.checkpoint().map_err(crate::fs::vfs::error::VfsError::from)?; // cancel/pause between blocks
        let n = std::io::Read::read(&mut stream, &mut buf).map_err(crate::fs::vfs::error::VfsError::from)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// The generic scan lane: engine-driven traversal over `read_dir` (pruned subtrees cost
/// zero round-trips), then a hashing pool sized to the backend's stream budget.
///
/// Error discipline, and the one place it differs from the local lane on purpose:
/// an entry-level NotFound is a scan race (counted + sampled, like local walk errors),
/// but a directory-level Transient/Auth/Protocol failure aborts the whole scan —
/// a half table would make the missing half read as deletions on the next compare.
fn scan_vfs(
    vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
    phase: crate::model::event::Phase,
) -> std::io::Result<Snapshot> {
    use crate::fs::vfs::error::VfsErrorKind;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    let pp = crate::obs::progress::PhaseProgress::begin(ctx, phase, Some(vfs.display()), 0, 0);
    let side = match phase {
        crate::model::event::Phase::ScanTarget => "target",
        _ => "source",
    };
    let started = now_ms();
    let t0 = std::time::Instant::now();
    let identity = vfs.identity();
    let caps = vfs.caps();
    // A backend without ranged reads cannot sample — the tier upgrades to full reads.
    // Never silent: preflight already put a NeedsAck line in front of the user, and the
    // snapshot's VfsNote records the tier that actually ran.
    let sampled = opt.sampled && caps.ranged_read.yes();
    let cache = if opt.hash && opt.use_cache { crate::store::hashcache::load_by_key(&identity) } else { HashMap::new() };
    let mtime_fixes = crate::store::mtimefix::load_by_key(&identity);

    let mut entries: Vec<Entry> = Vec::new();
    struct PendingVfs {
        rel: String,
        size: u64,
        mt: i64,
        hash: Option<String>,
        file_id: Option<String>,
        mode: Option<u32>,
    }
    let mut pending: Vec<PendingVfs> = Vec::new();
    let mut walk_errors = 0u64;
    let mut walk_err_samples: Vec<String> = Vec::new();
    let mut excl_dirs = 0u64;
    let mut excl_files = 0u64;

    // Engine-driven DFS: one read_dir round-trip per kept directory
    let mut stack: Vec<String> = vec![String::new()];
    while let Some(dir) = stack.pop() {
        pp.checkpoint()?;
        let list = match vfs.read_dir(&dir) {
            Ok(l) => l,
            Err(e) if e.kind == VfsErrorKind::NotFound && !dir.is_empty() => {
                // The directory vanished between being listed and being read: a scan race,
                // same class as a local walk error
                walk_errors += 1;
                if walk_err_samples.len() < 5 {
                    walk_err_samples.push(format!("{dir}: {e}"));
                }
                continue;
            }
            Err(e) => {
                let ioe: std::io::Error = e.into();
                return Err(std::io::Error::new(
                    ioe.kind(),
                    format!(
                        "scan of '{}' aborted at directory '{dir}': {ioe} — refusing to emit a half table (its missing subtrees would read as deletions)",
                        vfs.display()
                    ),
                ));
            }
        };
        for de in list {
            let rel = if dir.is_empty() { de.name.clone() } else { format!("{dir}/{}", de.name) };
            match de.meta.kind {
                EntryKind::Dir => {
                    let (pass, child_might_match) = opt.filter.pass_dir(&rel);
                    if pass {
                        entries.push(Entry { path: rel.clone(), kind: EntryKind::Dir, size: 0, mtime_ms: de.meta.mtime_ms, hash: None, file_id: None, mode: None, link: None, prev: None });
                    }
                    if pass || child_might_match {
                        stack.push(rel);
                    } else {
                        excl_dirs += 1; // whole subtree pruned — and never even listed
                    }
                }
                EntryKind::Symlink => {
                    if !opt.filter.pass_file(&rel) {
                        excl_files += 1;
                        continue;
                    }
                    if opt.symlinks_direct {
                        let target = de.meta.link.clone().or_else(|| vfs.read_link(&rel).ok());
                        entries.push(Entry { path: rel, kind: EntryKind::Symlink, size: 0, mtime_ms: de.meta.mtime_ms, hash: None, file_id: None, mode: None, link: target, prev: None });
                    }
                }
                EntryKind::File => {
                    if !opt.filter.pass_file(&rel) {
                        excl_files += 1;
                        continue;
                    }
                    let size = de.meta.size;
                    let raw_mt = de.meta.mtime_ms;
                    let mt = match mtime_fixes.get(&rel) {
                        Some((ondisk, intended)) if *ondisk == raw_mt => *intended,
                        _ => raw_mt,
                    };
                    let mut hash = None;
                    if opt.hash && opt.use_cache {
                        if let Some((cs, cm, ch)) = cache.get(&rel) {
                            let want_sampled = sampled && size >= SAMPLE_MIN;
                            if *cs == size && *cm == mt && ch.starts_with('~') == want_sampled {
                                hash = Some(ch.clone());
                            }
                        }
                    }
                    pending.push(PendingVfs { rel, size, mt, hash, file_id: de.meta.file_id, mode: de.meta.mode });
                    pp.item_done(&pending.last().unwrap().rel);
                }
            }
        }
    }

    if walk_errors > 0 {
        crate::log_warn!(
            "scan",
            "warning: {walk_errors} entr(ies) under {} skipped by walk errors — they will look ABSENT on this side! samples: {}",
            vfs.display(),
            walk_err_samples.join(" | ")
        );
        pp.error(
            "",
            "walk",
            side,
            &format!("{walk_errors} entr(ies) skipped by walk errors (they will be treated as ABSENT on this side!) samples: {}", walk_err_samples.join(" | ")),
        );
    }

    let bytes_to_hash: u64 = if opt.hash {
        pending.iter().filter(|p| p.hash.is_none()).map(|p| effective_read(p.size, sampled)).sum()
    } else {
        0
    };
    pp.set_totals(pending.len() as u64, bytes_to_hash);
    pp.restart_items();

    let hash_errors;
    if opt.hash {
        // Not rayon: the bottleneck is the network, and the width belongs to the backend
        // (its connection budget), not to the CPU count
        let width = caps.max_parallel_streams.clamp(1, 4);
        let next = AtomicUsize::new(0);
        let err_count = AtomicU64::new(0);
        let hashes: Vec<std::sync::OnceLock<Option<String>>> =
            pending.iter().map(|_| std::sync::OnceLock::new()).collect();
        std::thread::scope(|sc| {
            for _ in 0..width {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = pending.get(i) else { break };
                    if pp.checkpoint().is_err() {
                        let _ = hashes[i].set(None);
                        continue; // cancelled: drain the remaining slots empty
                    }
                    if p.hash.is_some() {
                        pp.item_done(&p.rel); // cache hit: nothing to read
                        let _ = hashes[i].set(None);
                        continue;
                    }
                    let res = if sampled && p.size >= SAMPLE_MIN {
                        sampled_digest_vfs(vfs.as_ref(), &p.rel, p.size)
                    } else {
                        full_hash_vfs(vfs.as_ref(), &p.rel, &pp)
                    };
                    match res {
                        Ok(h) => {
                            let _ = hashes[i].set(Some(h));
                        }
                        Err(e) if e.kind == VfsErrorKind::Cancelled => {
                            let _ = hashes[i].set(None);
                            continue;
                        }
                        Err(e) => {
                            err_count.fetch_add(1, Ordering::Relaxed);
                            pp.error(&p.rel, "hash", side, &e.to_string());
                            let _ = hashes[i].set(None);
                        }
                    }
                    let eff = effective_read(p.size, sampled);
                    pp.add_bytes(eff, &p.rel);
                    pp.item_done(&p.rel);
                });
            }
        });
        pp.checkpoint()?; // a cancellation during hashing surfaces here, honestly
        for (p, slot) in pending.iter_mut().zip(hashes) {
            if p.hash.is_none() {
                p.hash = slot.into_inner().flatten();
            }
        }
        hash_errors = err_count.load(Ordering::Relaxed);
    } else {
        hash_errors = 0;
    }

    for p in pending {
        entries.push(Entry { path: p.rel, kind: EntryKind::File, size: p.size, mtime_ms: p.mt, hash: p.hash, file_id: p.file_id, mode: p.mode, link: None, prev: None });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    if opt.hash {
        crate::store::hashcache::save_by_key(&identity, &entries);
    }
    if hash_errors > 0 {
        crate::log_warn!("scan", "warning: {hash_errors} file(s) could not be hashed (in use / unreadable)");
    }

    Ok(Snapshot {
        header: Header {
            schema: SCHEMA,
            kind: "snapshot".into(),
            root: vfs.display(),
            host: crate::model::table::host_name(),
            os: caps.protocol.to_string(),
            scanned_at_ms: started,
            duration_ms: t0.elapsed().as_millis() as u64,
            entry_count: entries.len() as u64,
            hashed: opt.hash,
            excluded_dirs: excl_dirs,
            excluded_files: excl_files,
            vfs: Some(crate::model::table::VfsNote {
                protocol: caps.protocol.to_string(),
                display_root: vfs.display(),
                mtime_precision_ms: caps.mtime_precision_ms,
                evidence_effective: if !opt.hash {
                    "none".into()
                } else if sampled {
                    "sampled".into()
                } else {
                    "full".into()
                },
                name_rules: caps.name_rules.as_str().into(),
                degraded: Vec::new(),
            }),
        },
        entries,
    })
}

fn scan_impl(
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
    // parallel_scan: with many small files the bottleneck is serial I/O). Large files split further via mmap_rayon; small ones use a plain mmap to avoid oversubscription.
    struct PendingFile {
        rel: String,
        abs: std::path::PathBuf,
        size: u64,
        mt: i64,
        hash: Option<String>, // cache hit
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
            Err(e) => {
                walk_errors += 1;
                if walk_err_samples.len() < 5 {
                    walk_err_samples.push(format!(
                        "{}: {e}",
                        e.path().map(|p| p.display().to_string()).unwrap_or_default()
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
                entries.push(Entry { path: rel, kind: EntryKind::Dir, size: 0, mtime_ms: mtime_ms(&md), hash: None, file_id: None, mode: None, link: None, prev: None });
            }
        } else if item.file_type().is_symlink() {
            if opt.symlinks_direct {
                let target = std::fs::read_link(item.path())
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned());
                entries.push(Entry { path: rel, kind: EntryKind::Symlink, size: 0, mtime_ms: mtime_ms(&md), hash: None, file_id: None, mode: None, link: target, prev: None });
            }
        } else {
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
            pending.push(PendingFile { rel, abs: item.path().to_path_buf(), size, mt, hash, file_id: file_id(&md), mode: unix_mode(&md) });
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
        const BIG_FILE: u64 = 32 * 1024 * 1024;
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
            // Files ≥32MB are no longer mmap-hashed in one shot: that would make "cancel" wait for the in-flight large file
            // to finish reading (a OneDrive cloud placeholder would first have to hydrate whole, possibly a several-minute wait).
            // The chunked read loop puts a cancel/pause checkpoint every 8MiB; this gives up blake3's intra-file multicore,
            // but the outer file-level parallelism remains, the disk is almost always the bottleneck, and the throughput difference sinks into the noise.
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
                    let res = if p.size >= BIG_FILE {
                        (|| -> std::io::Result<()> {
                            use std::io::Read;
                            let mut f = std::fs::File::open(&p.abs)?;
                            let mut buf = vec![0u8; 8 * 1024 * 1024];
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
                        })()
                    } else {
                        hasher.update_mmap(&p.abs).map(|_| ())
                    };
                    match res {
                        Ok(_) => p.hash = Some(hasher.finalize().to_hex().to_string()),
                        Err(e) if crate::obs::progress::is_cancelled(&e) => return, // cancellation is not a hash error
                        Err(e) => {
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
        entries.push(Entry { path: p.rel, kind: EntryKind::File, size: p.size, mtime_ms: p.mt, hash: p.hash, file_id: p.file_id, mode: p.mode, link: None, prev: None });
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
            vfs: None,
        },
        entries,
    })
}

#[cfg(test)]
mod ctx_tests {
    use super::*;
    use crate::model::event::{Phase, ProgressEvent};
use crate::obs::progress::{is_cancelled, RunCtl, RunCtx};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    /// A name the filesystem accepts but Unicode does not. Recording it through
    /// `to_string_lossy` would put `b<U+FFFD>c` in the table — a path that resolves to
    /// nothing, so apply would miss the real file and mirror would read the original as a
    /// deletion. It must be skipped and counted, never spelled differently.
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

    fn mk_tree(tag: &str, n: usize) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("syncdash-scanctx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for i in 0..n {
            std::fs::write(root.join("sub").join(format!("f{i}.dat")), vec![i as u8; 100]).unwrap();
        }
        root
    }

    #[test]
    fn sampled_digest_catches_edge_edits_only() {
        let d = std::env::temp_dir().join(format!("sd-sample-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("s.bin");
        let mut data = vec![5u8; 8 * 1024 * 1024];
        std::fs::write(&f, &data).unwrap();
        let a = sampled_digest(&f, data.len() as u64).unwrap();
        assert!(a.starts_with('~'), "sampled digests carry the ~ marker");
        // The midpoint falls inside a sample window → the digest must change
        data[4 * 1024 * 1024 + 10] = 9;
        std::fs::write(&f, &data).unwrap();
        let b = sampled_digest(&f, data.len() as u64).unwrap();
        assert_ne!(a, b, "an edit inside a sample window must change the digest");
        // Offset 1MB lies outside all three sample windows → the digest does not change. That is fast mode's safety boundary; assert it honestly
        data[1024 * 1024] = 7;
        std::fs::write(&f, &data).unwrap();
        let c = sampled_digest(&f, data.len() as u64).unwrap();
        assert_eq!(b, c, "fast mode by design does not see edits outside sample windows");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn opts() -> ScanOptions {
        ScanOptions {
            hash: true,
            sampled: false,
            use_cache: false, // tests never eat the cache
            symlinks_direct: false,
            filter: crate::pipeline::filter::PathFilter::build(&[], &[]),
        }
    }

    #[test]
    fn scan_ctx_reports_exact_totals() {
        let root = mk_tree("totals", 20);
        let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let ctx = RunCtx::new(RunCtl::new(), Arc::new(move |ev| s2.lock().unwrap().push(ev)));
        let snap = scan_ctx(&root, &opts(), &ctx, Phase::ScanSource).unwrap();
        assert_eq!(snap.entries.iter().filter(|e| e.kind == EntryKind::File).count(), 20);
        let evs = store.lock().unwrap();
        let totals = evs.iter().find_map(|e| match e {
            ProgressEvent::Totals { items_total, bytes_total, .. } => Some((*items_total, *bytes_total)),
            _ => None,
        });
        assert_eq!(totals, Some((20, 2000)), "the end of the walk must yield exact totals");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_ctx_cancels_midway() {
        let root = mk_tree("cancel", 50);
        let ctl = RunCtl::new();
        let ctl2 = ctl.clone();
        let sink = move |ev: ProgressEvent| {
            if matches!(ev, ProgressEvent::Progress { .. }) {
                ctl2.cancel.store(true, Ordering::SeqCst); // call a halt on the very first progress event
            }
        };
        let ctx = RunCtx::new(ctl, Arc::new(sink));
        match scan_ctx(&root, &opts(), &ctx, Phase::ScanSource) {
            Err(e) => assert!(is_cancelled(&e)),
            Ok(_) => panic!("expected cancellation"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
