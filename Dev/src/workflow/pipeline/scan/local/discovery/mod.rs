//! Metadata discovery for a local root, independent from content evidence collection.

#[cfg(target_os = "macos")]
mod bulk;
mod walk;

use crate::model::table::{ObservedDirectory, ObservedEntry, ObservedSymlink};

use super::super::{ProgressFn, ScanOptions, ScanProgress};
use super::model::PendingFile;
use super::state::LocalScanState;
use walk::WalkKind;

#[cfg(target_os = "macos")]
use bulk::walk as walk_local;
#[cfg(not(target_os = "macos"))]
use walk::walk as walk_local;

pub(super) struct Discovery {
    pub entries: Vec<ObservedEntry>,
    pub pending_files: Vec<PendingFile>,
    pub excluded_dirs: u64,
    pub excluded_files: u64,
    pub walk_errors: u64,
    pub walk_error_samples: Vec<String>,
    pub icloud_stubs: u64,
    pub icloud_stub_samples: Vec<String>,
    pub dataless_files: u64,
    pub skipped_symlinks: u64,
}

fn is_icloud_stub(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".icloud") && name.len() > ".icloud".len() + 1
}

pub(super) fn discover(
    root: &crate::fs::local_root::LocalRoot,
    options: &ScanOptions,
    state: &mut LocalScanState,
    phase_progress: Option<&crate::obs::progress::PhaseProgress<'_>>,
    progress: Option<ProgressFn<'_>>,
) -> std::io::Result<Discovery> {
    let mut entries = Vec::new();
    let mut pending_files = Vec::new();
    let mut icloud_stubs = 0;
    let mut icloud_stub_samples = Vec::new();
    let mut dataless_files = 0;
    let mut skipped_symlinks = 0;
    let mut symlink_read_error = None;
    let walk_started = std::time::Instant::now();
    let mut last_progress_sample = None;

    let stats = walk_local(
        root,
        &options.filter,
        || match phase_progress {
            Some(progress) => progress.checkpoint(),
            None => Ok(()),
        },
        |item| match item.kind {
            WalkKind::Dir => {
                if options.filter.pass_dir(item.relative.as_str()).0 {
                    entries.push(ObservedEntry::Directory(ObservedDirectory {
                        path: item.relative,
                        mtime_ms: item.mtime_ms,
                    }));
                }
            }
            WalkKind::Symlink => {
                if !options.symlinks_direct {
                    skipped_symlinks += 1;
                } else {
                    match root.read_link(&item.relative) {
                        Ok(target) => entries.push(ObservedEntry::Symlink(ObservedSymlink {
                            path: item.relative,
                            mtime_ms: item.mtime_ms,
                            target: target.to_string_lossy().into_owned(),
                        })),
                        Err(error) if symlink_read_error.is_none() => {
                            symlink_read_error = Some((item.relative, error));
                        }
                        Err(_) => {}
                    }
                }
            }
            WalkKind::File => {
                let relative_text = item.relative.as_str().to_owned();
                if is_icloud_stub(crate::foundation::path::base_name(&relative_text)) {
                    icloud_stubs += 1;
                    if icloud_stub_samples.len() < 5 {
                        icloud_stub_samples.push(relative_text);
                    }
                } else if item.dataless {
                    dataless_files += 1;
                }
                pending_files.push(state.prepare_file(
                    item.relative,
                    item.size,
                    item.mtime_ms,
                    item.file_id,
                    item.mode,
                    options,
                ));
                if let Some(progress) = phase_progress {
                    progress.item_done(pending_files.last().unwrap().relative.as_str());
                }
                if let Some(callback) = progress {
                    if super::progress::walk_sample_due(
                        &mut last_progress_sample,
                        walk_started.elapsed(),
                    ) {
                        callback(ScanProgress {
                            phase: "walk",
                            files_done: pending_files.len() as u64,
                            files_total: 0,
                            bytes_total: 0,
                            bytes_done: 0,
                            mib_per_s: 0.0,
                            complete: false,
                        });
                    }
                }
            }
        },
    )?;

    if let Some((relative, error)) = symlink_read_error {
        return Err(std::io::Error::new(
            error.kind(),
            format!(
                "could not read symlink target for {:?}: {error}",
                relative.as_str()
            ),
        ));
    }

    Ok(Discovery {
        entries,
        pending_files,
        excluded_dirs: stats.excluded_dirs,
        excluded_files: stats.excluded_files,
        walk_errors: stats.walk_errors,
        walk_error_samples: stats.walk_err_samples,
        icloud_stubs,
        icloud_stub_samples,
        dataless_files,
        skipped_symlinks,
    })
}
