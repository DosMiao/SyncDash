//! Bounded parallel content-evidence collection over discovered `Vfs` files.
//!
//! Not rayon, unlike the local lane: the bottleneck here is the network, and the width belongs to
//! the backend's connection budget rather than to the CPU count. Work is claimed from one shared
//! counter and each result lands in its own slot, so a worker that is cancelled mid-set cannot
//! shift another file's outcome onto a neighbour.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::fs::vfs::VfsEntryKind;
use crate::model::digest::Blake3Digest;

use super::super::digest::{effective_read, full_hash_vfs, sampled_digest_vfs, SAMPLE_MIN};
use super::super::model::PendingFile;
use super::super::{ScanMetrics, ScanOptions};

/// What the hashing pass produced for one pending entry.
///
/// Three outcomes, not two. `Skipped` (a cache hit, or a cancellation) and `Failed` both used to
/// collapse to `None`, which made a read failure indistinguishable from having nothing to do — and
/// that is precisely the distinction compare needs, because only one of them means the equality
/// judgment for that file has been silently downgraded to size and mtime.
enum SlotOutcome {
    Skipped,
    Done(Blake3Digest),
    Failed,
}

/// Where one collection pass reports what it is doing.
///
/// The two travel together because they are only meaningful together: the phase stream places an
/// event in the run, and `side` is what tells a reader which root a hash error happened on.
pub(super) struct ScanReporting<'a, 'phase> {
    pub phase: &'a crate::obs::progress::PhaseProgress<'phase>,
    pub side: &'a str,
}

/// Collect content evidence for every pending file that does not already carry a reusable digest,
/// and report how many could not be read.
///
/// `sampled` is the tier that will actually run rather than `options.sampled`, so a backend forced
/// up to full reads reads whole files here and labels them whole in `PendingFile::into_entry`.
pub(super) fn collect(
    vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    pending: &mut [PendingFile],
    options: &ScanOptions,
    sampled: bool,
    reporting: &ScanReporting<'_, '_>,
    max_parallel_streams: usize,
    metrics: &mut ScanMetrics,
) -> std::io::Result<u64> {
    use crate::fs::vfs::error::VfsErrorKind;

    let ScanReporting { phase: pp, side } = *reporting;
    let bytes_to_hash: u64 = if options.hash {
        pending
            .iter()
            .filter(|p| p.hash.is_none())
            .map(|p| effective_read(p.size, sampled))
            .sum()
    } else {
        0
    };
    if options.hash {
        pp.restart_items_with_totals(pending.len() as u64, bytes_to_hash);
    } else {
        pp.set_totals(pending.len() as u64, bytes_to_hash);
    }

    let hash_errors;
    let actual_bytes_read = AtomicU64::new(0);
    let measured = std::time::Instant::now();
    if options.hash {
        let width = max_parallel_streams.clamp(1, 4);
        metrics.workers = width;
        let next = AtomicUsize::new(0);
        let err_count = AtomicU64::new(0);
        let hashes: Vec<std::sync::OnceLock<SlotOutcome>> =
            pending.iter().map(|_| std::sync::OnceLock::new()).collect();
        std::thread::scope(|sc| {
            for _ in 0..width {
                sc.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = pending.get(i) else { break };
                    if pp.checkpoint().is_err() {
                        let _ = hashes[i].set(SlotOutcome::Skipped);
                        continue; // cancelled: drain the remaining slots empty
                    }
                    if p.hash.is_some() {
                        pp.item_done(p.relative.as_str()); // cache hit: nothing to read
                        let _ = hashes[i].set(SlotOutcome::Skipped);
                        continue;
                    }
                    let res = if sampled && p.size >= SAMPLE_MIN {
                        sampled_digest_vfs(vfs.as_ref(), p.relative.as_str(), p.size, |n| {
                            actual_bytes_read.fetch_add(n, Ordering::Relaxed);
                            pp.add_bytes(n, p.relative.as_str());
                        })
                    } else {
                        full_hash_vfs(vfs.as_ref(), p.relative.as_str(), p.size, pp, |n| {
                            actual_bytes_read.fetch_add(n, Ordering::Relaxed);
                            pp.add_bytes(n, p.relative.as_str());
                        })
                    }
                    .and_then(|hash| match vfs.stat(p.relative.as_str())? {
                        Some(meta)
                            if meta.kind == VfsEntryKind::File
                                && meta.size == p.size
                                && meta.mtime_ms == p.raw_mtime_ms =>
                        {
                            Ok(hash)
                        }
                        _ => Err(crate::fs::vfs::error::VfsError::new(
                            VfsErrorKind::Io,
                            "file changed while content evidence was being read",
                        )),
                    });
                    match res {
                        Ok(h) => {
                            let _ = hashes[i].set(SlotOutcome::Done(h));
                        }
                        Err(e) if e.kind == VfsErrorKind::Cancelled => {
                            let _ = hashes[i].set(SlotOutcome::Skipped);
                            continue;
                        }
                        Err(e) => {
                            err_count.fetch_add(1, Ordering::Relaxed);
                            pp.error(p.relative.as_str(), "hash", side, &e.to_string());
                            let _ = hashes[i].set(SlotOutcome::Failed);
                        }
                    }
                    pp.item_done(p.relative.as_str());
                });
            }
        });
        pp.checkpoint()?; // a cancellation during hashing surfaces here, honestly
        for (p, slot) in pending.iter_mut().zip(hashes) {
            match slot.into_inner() {
                Some(SlotOutcome::Done(h)) => p.hash = Some(h),
                Some(SlotOutcome::Failed) => p.hash_failed = true,
                // A cache hit already carries its hash; a cancellation leaves the entry alone.
                Some(SlotOutcome::Skipped) | None => {}
            }
        }
        hash_errors = err_count.load(Ordering::Relaxed);
    } else {
        hash_errors = 0;
    }
    metrics.hash_ms = measured.elapsed().as_millis() as u64;
    metrics.read_bytes = actual_bytes_read.load(Ordering::Relaxed);
    Ok(hash_errors)
}
