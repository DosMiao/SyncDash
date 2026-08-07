//! Generic-lane façade: discover metadata, collect content evidence, and publish one snapshot.
//!
//! Every protocol backend rides this code. It has no local fast path available to it, so it
//! streams, and it must produce a snapshot indistinguishable from the local lane's for the same
//! content — including the exclusion counts, which the UI reports.
//!
//! Same shape as `scan::local`, for the same reason: traversal and content reads change for
//! unrelated causes, so `discovery` owns the walk and `hashing` owns the reads, while this façade
//! alone owns their ordering, the acceleration-table lifetime, and the snapshot contract.

mod discovery;
mod hashing;

use std::collections::{HashMap, HashSet};

use crate::foundation::time::now_ms;
use crate::model::table::{TableArtifact, TableHeader, TableKind, TABLE_SCHEMA};

use super::{ScanMetrics, ScanOptions};

/// The generic scan lane: engine-driven traversal over `read_dir` (pruned subtrees cost
/// zero round-trips), then a hashing pool sized to the backend's stream budget.
pub(super) fn scan_vfs(
    vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
    phase: crate::model::event::Phase,
) -> std::io::Result<TableArtifact> {
    let pp = crate::obs::progress::PhaseProgress::begin(ctx, phase, Some(vfs.display()), 0, 0);
    let side = match phase {
        crate::model::event::Phase::ScanTarget => "target",
        _ => "source",
    };
    let started = now_ms();
    let t0 = std::time::Instant::now();
    let mut metrics = ScanMetrics::default();
    let identity = vfs.identity();
    let caps = vfs.caps();
    // A backend without ranged reads cannot sample — the tier upgrades to full reads. Never
    // silent: preflight already put a NeedsAck line in front of the user, and `header.evidence`
    // below records the tier that actually ran rather than the one that was asked for. (The
    // downgrade sentence itself lands in `VfsNote.degraded`, but only on the compare path —
    // `run::local::compare` fills it from the caps report; archive refresh and the peer lane do
    // not.) `run::effective_scan_opts` normally clears `sampled` before this lane is entered; this
    // is the guard that keeps a caller who does not from stamping Unreadable on every file ≥4MB,
    // because a backend like ftp refuses `read_range` outright.
    let sampled = opt.sampled && caps.ranged_read.yes();
    let measured = std::time::Instant::now();
    // A no-cache scan must not reuse these hashes, but it still needs the previous table in order
    // to retain rows deliberately outside the active filter domain when reconciliation completes.
    let cache = if opt.hash {
        crate::store::hashcache::load_by_key(&identity)
    } else {
        HashMap::new()
    };
    metrics.cache_load_ms = measured.elapsed().as_millis() as u64;
    let measured = std::time::Instant::now();
    let mtime_fixes = crate::store::mtimefix::load_by_key(&identity);
    let mut matched_mtime_fixes = HashSet::new();
    metrics.mtime_load_ms = measured.elapsed().as_millis() as u64;

    let measured = std::time::Instant::now();
    let mut discovered = discovery::discover(
        vfs,
        opt,
        sampled,
        &cache,
        &mtime_fixes,
        &mut matched_mtime_fixes,
        &pp,
    )?;
    metrics.walk_ms = measured.elapsed().as_millis() as u64;
    metrics.files = discovered.pending_files.len() as u64;
    metrics.cache_hits = discovered
        .pending_files
        .iter()
        .filter(|file| file.hash.is_some())
        .count() as u64;
    // An unread subtree makes this scan partial for the same reason a walk error does: the cache
    // must not retire rows for paths this round was never allowed to look at.
    let coverage = if discovered.walk_errors == 0 && discovered.unread_paths.is_empty() {
        crate::store::ScanCoverage::Complete
    } else {
        crate::store::ScanCoverage::Partial
    };
    let retain_absent = super::state::retain_absent(&cache, &mtime_fixes, coverage, &opt.filter);

    if discovered.walk_errors > 0 {
        crate::log_warn!(
            "scan",
            "warning: {} entr(ies) under {} skipped by walk errors — they will look ABSENT on this side! samples: {}",
            discovered.walk_errors,
            vfs.display(),
            discovered.walk_error_samples.join(" | ")
        );
        pp.error(
            "",
            "walk",
            side,
            &format!(
                "{} entr(ies) skipped by walk errors (they will be treated as ABSENT on this side!) samples: {}",
                discovered.walk_errors,
                discovered.walk_error_samples.join(" | ")
            ),
        );
    }

    let hash_errors = hashing::collect(
        vfs,
        &mut discovered.pending_files,
        opt,
        sampled,
        &hashing::ScanReporting { phase: &pp, side },
        caps.max_parallel_streams,
        &mut metrics,
    )?;

    let measured = std::time::Instant::now();
    let mut entries = discovered.entries;
    entries.extend(
        discovered
            .pending_files
            .into_iter()
            .map(|file| file.into_entry(sampled)),
    );
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    metrics.finalize_ms = measured.elapsed().as_millis() as u64;
    let measured = std::time::Instant::now();
    if opt.hash
        && crate::store::hashcache::save_by_key(&identity, &entries, coverage, &retain_absent)
            == crate::store::StateWriteStatus::Failed
    {
        metrics.state_failures += 1;
    }
    if crate::store::mtimefix::reconcile_by_key(
        &identity,
        &mtime_fixes,
        &entries,
        coverage,
        &matched_mtime_fixes,
        &retain_absent,
    ) == crate::store::StateWriteStatus::Failed
    {
        metrics.state_failures += 1;
    }
    metrics.state_write_ms = measured.elapsed().as_millis() as u64;
    if hash_errors > 0 {
        crate::log_warn!(
            "scan",
            "warning: {hash_errors} file(s) could not be hashed (in use / unreadable)"
        );
    }

    let snapshot = TableArtifact::new(
        TableHeader {
            schema: TABLE_SCHEMA,
            kind: TableKind::Snapshot,
            root: vfs.display(),
            host: crate::foundation::machine::host_name(),
            os: caps.protocol.to_string(),
            scanned_at_ms: started,
            duration_ms: t0.elapsed().as_millis() as u64,
            entry_count: entries.len() as u64,
            evidence: super::evidence_tier(opt.hash, sampled),
            excluded_dirs: discovered.excluded_dirs,
            excluded_files: discovered.excluded_files,
            walk_errors: discovered.walk_errors,
            walk_err_samples: discovered.walk_error_samples,
            unread_paths: discovered.unread_paths,
            // iCloud eviction is a property of a local macOS filesystem. A root reached over
            // sftp/smb/ftp has no such state to report, and inventing a zero that means "checked
            // and found none" would be a different claim from "not applicable here".
            icloud_stubs: 0,
            icloud_stub_samples: Vec::new(),
            dataless_files: 0,
            skipped_symlinks: discovered.skipped_symlinks,
            vfs: Some(super::vfs_note(vfs.as_ref())),
        },
        entries,
    )?;
    metrics.emit(ctx, side, "vfs");
    pp.finish()?;
    Ok(snapshot)
}
