//! Local scan façade: discover metadata, collect content evidence, and publish one snapshot.
//!
//! The three stages have separate change axes: platform traversal lives in `discovery`, stable
//! content reads in `hashing`, and persistent acceleration tables in `state`. This façade alone
//! owns their ordering and the externally visible snapshot contract.

mod discovery;
mod hashing;
mod model;
mod progress;
mod state;

#[cfg(test)]
mod tests;

use crate::foundation::time::now_ms;
use crate::fs::local_root::LocalRoot;
use crate::model::table::{TableArtifact, TableEvidence, TableHeader, TableKind, TABLE_SCHEMA};

use super::{ProgressFn, ScanMetrics, ScanOptions, ScanProgress};

pub(crate) fn scan_local_root_impl(
    root: &LocalRoot,
    options: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
    context: Option<(&crate::obs::progress::RunCtx, crate::model::event::Phase)>,
    max_parallel_streams: usize,
) -> std::io::Result<TableArtifact> {
    let phase_progress = context.map(|(run, phase)| {
        crate::obs::progress::PhaseProgress::begin(
            run,
            phase,
            Some(root.display_path().to_string_lossy().into_owned()),
            0,
            0,
        )
    });
    let side = match context.map(|(_, phase)| phase) {
        Some(crate::model::event::Phase::ScanTarget) => "target",
        _ => "source",
    };
    let scanned_at_ms = now_ms();
    let started = std::time::Instant::now();
    let mut metrics = ScanMetrics::default();
    let mut state = state::LocalScanState::load(root, options, &mut metrics);

    let measured = std::time::Instant::now();
    let mut discovered =
        discovery::discover(root, options, &mut state, phase_progress.as_ref(), progress)?;
    metrics.walk_ms = measured.elapsed().as_millis() as u64;
    metrics.files = discovered.pending_files.len() as u64;
    metrics.cache_hits = discovered
        .pending_files
        .iter()
        .filter(|file| file.hash.is_some())
        .count() as u64;

    if discovered.walk_errors > 0 {
        crate::log_warn!(
            "scan",
            "warning: {} entr(ies) under {} skipped by walk errors — they will look ABSENT on this side! samples: {}",
            discovered.walk_errors,
            root.display_path().display(),
            discovered.walk_error_samples.join(" | ")
        );
        if let Some(phase_progress) = &phase_progress {
            phase_progress.error(
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
    }

    let coverage = if discovered.walk_errors == 0 {
        crate::store::ScanCoverage::Complete
    } else {
        crate::store::ScanCoverage::Partial
    };
    let retain_absent = state.retain_absent(coverage, options);
    let hash = hashing::collect(
        root,
        &mut discovered.pending_files,
        options,
        phase_progress.as_ref(),
        progress,
        side,
        max_parallel_streams,
        &mut metrics,
    )?;
    if hash.errors > 0 {
        crate::log_warn!(
            "scan",
            "warning: {} file(s) could not be hashed (in use / unreadable)",
            hash.errors
        );
    }

    let measured = std::time::Instant::now();
    discovered.entries.extend(
        discovered
            .pending_files
            .into_iter()
            .map(|file| file.into_entry(options.sampled)),
    );
    discovered
        .entries
        .sort_by(|left, right| left.path().cmp(right.path()));
    metrics.finalize_ms = measured.elapsed().as_millis() as u64;

    let measured = std::time::Instant::now();
    state.publish(
        &discovered.entries,
        coverage,
        &retain_absent,
        options,
        &mut metrics,
    );
    metrics.state_write_ms = measured.elapsed().as_millis() as u64;

    let snapshot = TableArtifact::new(
        TableHeader {
            schema: TABLE_SCHEMA,
            kind: TableKind::Snapshot,
            root: root.display_path().to_string_lossy().into_owned(),
            host: crate::foundation::machine::host_name(),
            os: crate::foundation::machine::os_name(),
            scanned_at_ms,
            duration_ms: started.elapsed().as_millis() as u64,
            entry_count: discovered.entries.len() as u64,
            evidence: if !options.hash {
                TableEvidence::None
            } else if options.sampled {
                TableEvidence::Sampled
            } else {
                TableEvidence::Full
            },
            excluded_dirs: discovered.excluded_dirs,
            excluded_files: discovered.excluded_files,
            walk_errors: discovered.walk_errors,
            walk_err_samples: discovered.walk_error_samples,
            icloud_stubs: discovered.icloud_stubs,
            icloud_stub_samples: discovered.icloud_stub_samples,
            dataless_files: discovered.dataless_files,
            skipped_symlinks: discovered.skipped_symlinks,
            vfs: None,
        },
        discovered.entries,
    )?;
    if let Some((run, _)) = context {
        metrics.emit(run, side, "local");
    }
    if let Some(phase_progress) = phase_progress {
        phase_progress.finish()?;
    }
    if let Some(callback) = progress {
        callback(ScanProgress {
            phase: if options.hash { "hash" } else { "walk" },
            files_done: metrics.files,
            files_total: metrics.files,
            bytes_total: hash.bytes_to_hash,
            bytes_done: hash.bytes_done,
            mib_per_s: 0.0,
            complete: true,
        });
    }
    Ok(snapshot)
}
