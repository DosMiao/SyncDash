//! Bounded parallel content-evidence collection over discovered local files.

pub(super) mod reader;

use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use super::super::digest::{effective_read, SAMPLE_MIN};
use super::super::{ProgressFn, ScanMetrics, ScanOptions, ScanProgress};
use super::model::PendingFile;

pub(super) struct HashOutcome {
    pub bytes_to_hash: u64,
    pub bytes_done: u64,
    pub errors: u64,
}

pub(super) fn collect(
    root: &crate::fs::local_root::LocalRoot,
    pending: &mut [PendingFile],
    options: &ScanOptions,
    phase_progress: Option<&crate::obs::progress::PhaseProgress<'_>>,
    progress: Option<ProgressFn<'_>>,
    side: &str,
    max_parallel_streams: usize,
    metrics: &mut ScanMetrics,
) -> std::io::Result<HashOutcome> {
    let bytes_to_hash = if options.hash {
        pending
            .iter()
            .filter(|file| file.hash.is_none())
            .map(|file| effective_read(file.size, options.sampled))
            .sum()
    } else {
        0
    };
    metrics.workers = if options.hash {
        max_parallel_streams
            .max(1)
            .min(rayon::current_num_threads())
    } else {
        0
    };
    if let Some(progress) = phase_progress {
        if options.hash {
            progress.restart_items_with_totals(pending.len() as u64, bytes_to_hash);
        } else {
            progress.set_totals(pending.len() as u64, bytes_to_hash);
        }
    }

    let errors = AtomicU64::new(0);
    let files_done = AtomicU64::new(0);
    let bytes_done = AtomicU64::new(0);
    let measured = std::time::Instant::now();
    if options.hash {
        let files_total = pending.len() as u64;
        let global_width = rayon::current_num_threads();
        let width = max_parallel_streams.max(1).min(global_width);
        let pool = if width < global_width {
            Some(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(width)
                    .thread_name(|index| format!("sd-scan-{index}"))
                    .start_handler(|_| crate::foundation::thread::lower_priority())
                    .build()
                    .map_err(|error| {
                        std::io::Error::other(format!(
                            "cannot build {width}-thread scan pool: {error}"
                        ))
                    })?,
            )
        } else {
            None
        };

        if let Some(callback) = progress {
            callback(ScanProgress {
                phase: "hash",
                files_done: 0,
                files_total,
                bytes_total: bytes_to_hash,
                bytes_done: 0,
                mib_per_s: 0.0,
                complete: false,
            });
        }

        std::thread::scope(|scope| {
            let progress_stop = progress.map(|callback| {
                let (stop_tx, stop_rx) = std::sync::mpsc::channel();
                let (files, bytes, started) = (&files_done, &bytes_done, std::time::Instant::now());
                scope.spawn(move || {
                    while super::progress::hash_sample_due(&stop_rx) {
                        let completed_files = files.load(Ordering::Relaxed);
                        let completed_bytes = bytes.load(Ordering::Relaxed);
                        let seconds = started.elapsed().as_secs_f64().max(0.001);
                        callback(ScanProgress {
                            phase: "hash",
                            files_done: completed_files,
                            files_total,
                            bytes_total: bytes_to_hash,
                            bytes_done: completed_bytes,
                            mib_per_s: completed_bytes as f64 / seconds / (1024.0 * 1024.0),
                            complete: false,
                        });
                    }
                });
                stop_tx
            });

            let mut hash_all = || {
                pending
                    .par_iter_mut()
                    .for_each_init(Vec::new, |buffer, file| {
                        if let Some(progress) = phase_progress {
                            if progress.checkpoint().is_err() {
                                return;
                            }
                        }
                        if file.hash.is_none() {
                            let result = if options.sampled && file.size >= SAMPLE_MIN {
                                reader::sampled_hash_with_buffer(
                                    root,
                                    &file.relative,
                                    file.size,
                                    file.raw_mtime_ms,
                                    file.observed_file_id.as_deref(),
                                    buffer,
                                    |count| {
                                        bytes_done.fetch_add(count, Ordering::Relaxed);
                                        if let Some(progress) = phase_progress {
                                            progress.add_bytes(count, file.relative.as_str());
                                        }
                                    },
                                )
                            } else {
                                reader::full_hash_with_buffer(
                                    root,
                                    &file.relative,
                                    file.size,
                                    file.raw_mtime_ms,
                                    file.observed_file_id.as_deref(),
                                    buffer,
                                    || match phase_progress {
                                        Some(progress) => progress.checkpoint(),
                                        None => Ok(()),
                                    },
                                    |count| {
                                        bytes_done.fetch_add(count, Ordering::Relaxed);
                                        if let Some(progress) = phase_progress {
                                            progress.add_bytes(count, file.relative.as_str());
                                        }
                                    },
                                )
                            };
                            match result {
                                Ok(digest) => file.hash = Some(digest),
                                Err(error) if crate::obs::progress::is_cancelled(&error) => return,
                                Err(error) => {
                                    file.hash_failed = true;
                                    errors.fetch_add(1, Ordering::Relaxed);
                                    if let Some(progress) = phase_progress {
                                        progress.error(
                                            file.relative.as_str(),
                                            "hash",
                                            side,
                                            &error.to_string(),
                                        );
                                    }
                                }
                            }
                        }
                        if let Some(progress) = phase_progress {
                            progress.item_done(file.relative.as_str());
                        }
                        files_done.fetch_add(1, Ordering::Relaxed);
                    })
            };
            if let Some(pool) = &pool {
                pool.install(hash_all);
            } else {
                hash_all();
            }
            if let Some(stop) = progress_stop {
                let _ = stop.send(());
            }
        });
        if let Some(progress) = phase_progress {
            progress.checkpoint()?;
        }
    }
    metrics.hash_ms = measured.elapsed().as_millis() as u64;
    metrics.read_bytes = bytes_done.load(Ordering::Relaxed);
    Ok(HashOutcome {
        bytes_to_hash,
        bytes_done: metrics.read_bytes,
        errors: errors.load(Ordering::Relaxed),
    })
}
