//! The generic lane: everything reached through the `Vfs` trait.
//!
//! Every protocol backend rides this code. It has no local fast path available to it, so it
//! streams, and it must produce a snapshot indistinguishable from the local lane's for the same
//! content — including the exclusion counts, which the UI reports.

use std::collections::HashMap;

use crate::foundation::time::now_ms;
use crate::model::table::{Entry, EntryKind, Header, Snapshot, SCHEMA};

use super::digest::{effective_read, full_hash_vfs, sampled_digest_vfs, SAMPLE_MIN};
use super::ScanOptions;

/// The generic scan lane: engine-driven traversal over `read_dir` (pruned subtrees cost
/// zero round-trips), then a hashing pool sized to the backend's stream budget.
///
/// Error discipline, and the one place it differs from the local lane on purpose:
/// an entry-level NotFound is a scan race (counted + sampled, like local walk errors),
/// but a directory-level Transient/Auth/Protocol failure aborts the whole scan —
/// a half table would make the missing half read as deletions on the next compare.
pub(super) fn scan_vfs(
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
            vfs: Some(super::vfs_note(vfs.as_ref(), opt, sampled)),
        },
        entries,
    })
}
