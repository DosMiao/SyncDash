//! Metadata discovery over the `Vfs` trait, independent from content evidence collection.
//!
//! Engine-driven DFS rather than a backend-provided walker: the engine decides which directories
//! are worth a round trip, so a pruned subtree costs none. The filter contract and the exclusion
//! accounting are the local lane's, entry for entry — `scan::tests` compares the two tables.

use std::collections::HashSet;

use crate::foundation::path::RootRelativePath;
use crate::fs::vfs::VfsEntryKind;
use crate::model::table::{ObservedDirectory, ObservedEntry, ObservedSymlink};
use crate::store::hashcache::HashCache;
use crate::store::mtimefix::MtimeCorrections;

use super::super::model::PendingFile;
use super::super::{state as rules, ScanOptions};

/// What one traversal found, before any byte of content has been read.
///
/// Seven fields against the local lane's ten: iCloud eviction, dataless files, and their samples
/// are properties of a local macOS filesystem, and a root reached over a protocol has no such
/// state to report.
pub(super) struct VfsDiscovery {
    pub entries: Vec<ObservedEntry>,
    pub pending_files: Vec<PendingFile>,
    pub excluded_dirs: u64,
    pub excluded_files: u64,
    pub walk_errors: u64,
    pub walk_error_samples: Vec<String>,
    pub skipped_symlinks: u64,
}

/// Walk the whole root, or refuse.
///
/// The error discipline is the one place this lane differs from the local one on purpose: an
/// entry-level NotFound is a scan race (counted and sampled, like local walk errors), but a
/// directory-level Transient/Auth/Protocol failure aborts the whole scan, because a half table
/// would make its missing half read as deletions on the next compare.
///
/// `sampled` is the tier that will actually run rather than `options.sampled`; see
/// `scan::state::reusable_cached_digest` for why a cache row is judged against it.
pub(super) fn discover(
    vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    options: &ScanOptions,
    sampled: bool,
    cache: &HashCache,
    mtime_fixes: &MtimeCorrections,
    matched_mtime_fixes: &mut HashSet<String>,
    phase_progress: &crate::obs::progress::PhaseProgress<'_>,
) -> std::io::Result<VfsDiscovery> {
    use crate::fs::vfs::error::VfsErrorKind;

    let mut found = VfsDiscovery {
        entries: Vec::new(),
        pending_files: Vec::new(),
        excluded_dirs: 0,
        excluded_files: 0,
        walk_errors: 0,
        walk_error_samples: Vec::new(),
        skipped_symlinks: 0,
    };

    let mut stack: Vec<String> = vec![String::new()];
    while let Some(dir) = stack.pop() {
        phase_progress.checkpoint()?;
        let list = match vfs.read_dir(&dir) {
            Ok(l) => l,
            Err(e) if e.kind == VfsErrorKind::NotFound && !dir.is_empty() => {
                // The directory vanished between being listed and being read: a scan race,
                // same class as a local walk error
                found.walk_errors += 1;
                if found.walk_error_samples.len() < 5 {
                    found.walk_error_samples.push(format!("{dir}: {e}"));
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
            let rel = if dir.is_empty() {
                de.name.as_str().to_owned()
            } else {
                format!("{dir}/{}", de.name)
            };
            let relative = RootRelativePath::try_from(rel.as_str()).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
            })?;
            match de.meta.kind {
                VfsEntryKind::Directory => {
                    let (pass, child_might_match) = options.filter.pass_dir(&rel);
                    if pass {
                        found
                            .entries
                            .push(ObservedEntry::Directory(ObservedDirectory {
                                path: relative,
                                mtime_ms: de.meta.mtime_ms,
                            }));
                    }
                    if pass || child_might_match {
                        stack.push(rel);
                    } else {
                        found.excluded_dirs += 1; // whole subtree pruned — and never even listed
                    }
                }
                VfsEntryKind::Symlink => {
                    if !options.filter.pass_file(&rel) {
                        found.excluded_files += 1;
                        continue;
                    }
                    if !options.symlinks_direct {
                        found.skipped_symlinks += 1;
                    }
                    if options.symlinks_direct {
                        let target = match de.meta.link {
                            Some(target) => target,
                            None => vfs.read_link(&rel).map_err(std::io::Error::from)?,
                        };
                        found.entries.push(ObservedEntry::Symlink(ObservedSymlink {
                            path: relative,
                            mtime_ms: de.meta.mtime_ms,
                            target,
                        }));
                    }
                }
                VfsEntryKind::File => {
                    if !options.filter.pass_file(&rel) {
                        found.excluded_files += 1;
                        continue;
                    }
                    let size = de.meta.size;
                    let raw_mtime_ms = de.meta.mtime_ms;
                    let mtime_ms =
                        rules::resolve_mtime(mtime_fixes, matched_mtime_fixes, &rel, raw_mtime_ms);
                    let hash = rules::reusable_cached_digest(
                        cache,
                        matched_mtime_fixes,
                        options,
                        sampled,
                        &rel,
                        size,
                        mtime_ms,
                    );
                    found.pending_files.push(PendingFile {
                        relative,
                        size,
                        raw_mtime_ms,
                        mtime_ms,
                        hash,
                        hash_failed: false,
                        observed_file_id: None,
                        file_id: de.meta.file_id,
                        mode: de.meta.mode,
                    });
                    phase_progress.item_done(found.pending_files.last().unwrap().relative.as_str());
                }
            }
        }
    }

    Ok(found)
}
