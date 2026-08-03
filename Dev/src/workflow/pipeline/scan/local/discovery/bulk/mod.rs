//! macOS directory enumeration with `getattrlistbulk(2)`.
//!
//! One call returns names and stat-equivalent metadata for many children. This removes the
//! `readdir` + `lstat` syscall pair per item that dominates trees containing hundreds of thousands
//! of small files, while preserving the local scanner's strict path and error contracts.

mod compatibility;
mod record;

#[cfg(test)]
mod tests;

use self::compatibility::*;
use self::record::*;
use super::walk::{WalkEntry, WalkKind, WalkStats};
use super::{as_directory, child_path, subtree_error};
use crate::foundation::path::{EntryName, RootRelativeDir};
use crate::fs::local_root::LocalRoot;
use crate::pipeline::filter::PathFilter;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

pub(super) fn walk<C, V>(
    root: &LocalRoot,
    filter: &PathFilter,
    checkpoint: C,
    visit: V,
) -> std::io::Result<WalkStats>
where
    C: FnMut() -> std::io::Result<()>,
    V: FnMut(WalkEntry),
{
    walk_with_bulk(root, filter, checkpoint, visit, system_bulk_read)
}

fn walk_with_bulk<C, V, B>(
    root: &LocalRoot,
    filter: &PathFilter,
    mut checkpoint: C,
    mut visit: V,
    mut bulk_read: B,
) -> std::io::Result<WalkStats>
where
    C: FnMut() -> std::io::Result<()>,
    V: FnMut(WalkEntry),
    B: FnMut(RawFd, &mut [u8]) -> std::io::Result<usize>,
{
    let mut emitted = false;
    let result = {
        let mut emit = |entry| {
            emitted = true;
            visit(entry);
        };
        walk_bulk(root, filter, &mut checkpoint, &mut emit, &mut bulk_read)
    };

    match result {
        Err(error) if !emitted && is_root_bulk_compatibility_error(&error) => {
            crate::log_warn!(
                "scan",
                "{}; falling back to the compatible WalkDir enumerator before publishing any entries",
                error
            );
            super::walk::walk(root, filter, checkpoint, visit)
        }
        result => result,
    }
}

fn walk_bulk<C, V, B>(
    root: &LocalRoot,
    filter: &PathFilter,
    checkpoint: &mut C,
    visit: &mut V,
    bulk_read: &mut B,
) -> std::io::Result<WalkStats>
where
    C: FnMut() -> std::io::Result<()>,
    V: FnMut(WalkEntry),
    B: FnMut(RawFd, &mut [u8]) -> std::io::Result<usize>,
{
    let mut stats = WalkStats::default();
    let mut directories = vec![RootRelativeDir::new("").expect("the root directory is valid")];
    let mut buffer = vec![0u8; BUFFER_SIZE];

    while let Some(directory_relative) = directories.pop() {
        checkpoint()?;
        let directory = match root.open_directory(&directory_relative) {
            Ok(directory) => directory,
            Err(error) if directory_relative.as_str().is_empty() => {
                return Err(crate::pipeline::scan::root_unreadable_error(
                    root.display_path(),
                    error,
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                stats.note_error(format!("{}: {error}", directory_relative.as_str()));
                continue;
            }
            Err(error) => return Err(subtree_error(root, directory_relative.as_str(), error)),
        };
        let directory_handle = directory.try_clone_handle()?;

        loop {
            checkpoint()?;
            let count = match bulk_read(directory_handle.as_raw_fd(), &mut buffer) {
                Ok(count) => count,
                Err(error)
                    if directory_relative.as_str().is_empty()
                        && matches!(
                            error.raw_os_error(),
                            Some(libc::ENOTSUP) | Some(libc::EINVAL)
                        ) =>
                {
                    return Err(std::io::Error::new(
                        error.kind(),
                        RootBulkCompatibilityError {
                            root: root.display_path().to_path_buf(),
                            source: error,
                        },
                    ));
                }
                Err(error) => {
                    if directory_relative.as_str().is_empty() {
                        return Err(crate::pipeline::scan::root_unreadable_error(
                            root.display_path(),
                            error,
                        ));
                    }
                    if error.kind() == std::io::ErrorKind::NotFound {
                        stats.note_error(format!("{}: {error}", directory_relative.as_str()));
                        break;
                    }
                    return Err(subtree_error(root, directory_relative.as_str(), error));
                }
            };
            if count == 0 {
                break;
            }

            let mut offset = 0usize;
            for _ in 0..count {
                checkpoint()?;
                let record = record_at(&buffer, offset).map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "scan of '{}' received malformed directory metadata at '{}': {error} — refusing to emit a half table",
                            root.display_path().display(),
                            directory_relative.as_str()
                        ),
                    )
                })?;
                offset = offset
                    .checked_add(record.len())
                    .ok_or_else(|| std::io::Error::other("bulk record offset overflow"))?;
                let parsed = parse_record(record).map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "scan of '{}' received malformed directory metadata at '{}': {error} — refusing to emit a half table",
                            root.display_path().display(),
                            directory_relative.as_str()
                        ),
                    )
                })?;
                if parsed.name == b"." || parsed.name == b".." {
                    continue;
                }

                let name = std::ffi::OsString::from_vec(parsed.name.to_vec());
                let Some(name_text) = name.to_str() else {
                    stats.note_invalid_name(Path::new(&name));
                    continue;
                };
                let name = match EntryName::new(name_text) {
                    Ok(name) => name,
                    Err(error) => {
                        stats.note_error(error.to_string());
                        continue;
                    }
                };
                let relative = child_path(&directory_relative, &name);
                let mut fallback_metadata = None;
                let kind = match parsed.kind {
                    Some(kind) => kind,
                    None => match root.metadata_path(&relative) {
                        Ok(metadata) => {
                            let kind = super::walk::kind_from_metadata(&metadata);
                            fallback_metadata = Some(metadata);
                            kind
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            stats.note_error(format!("{}: {error}", relative.as_str()));
                            continue;
                        }
                        Err(error) => return Err(subtree_error(root, relative.as_str(), error)),
                    },
                };

                let keep = match kind {
                    WalkKind::Dir => {
                        let (pass, child_might_match) = filter.pass_dir(relative.as_str());
                        let keep = pass || child_might_match;
                        if !keep {
                            stats.excluded_dirs += 1;
                        }
                        keep
                    }
                    WalkKind::File | WalkKind::Symlink => {
                        let keep = filter.pass_file(relative.as_str());
                        if !keep {
                            stats.excluded_files += 1;
                        }
                        keep
                    }
                };
                if !keep {
                    continue;
                }

                let entry = if parsed.complete() {
                    parsed.into_walk_entry(relative.clone())
                } else {
                    let metadata = match fallback_metadata {
                        Some(metadata) => metadata,
                        None => match root.metadata_path(&relative) {
                            Ok(metadata) => metadata,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                stats.note_error(format!("{}: {error}", relative.as_str()));
                                if kind == WalkKind::Dir {
                                    directories.push(as_directory(relative.clone()));
                                }
                                continue;
                            }
                            Err(error) if kind != WalkKind::Dir => {
                                stats.note_error(format!("{}: {error}", relative.as_str()));
                                continue;
                            }
                            Err(error) => {
                                return Err(subtree_error(root, relative.as_str(), error));
                            }
                        },
                    };
                    let dataless = if kind == WalkKind::File {
                        root.is_dataless_file(&relative)?
                    } else {
                        false
                    };
                    WalkEntry::from_metadata(relative.clone(), &metadata, dataless)
                };
                visit(entry);

                if kind == WalkKind::Dir {
                    directories.push(as_directory(relative));
                }
            }
        }
    }

    Ok(stats)
}
