//! Descriptor-relative metadata traversal for the local scanner.

use std::path::Path;

use crate::foundation::path::{RootRelativeDir, RootRelativePath};
use crate::fs::local_root::LocalRoot;
use crate::pipeline::filter::PathFilter;

use super::{as_directory, child_path, subtree_error};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum WalkKind {
    Dir,
    File,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WalkEntry {
    pub relative: RootRelativePath,
    pub kind: WalkKind,
    pub size: u64,
    pub mtime_ms: i64,
    pub file_id: Option<String>,
    pub mode: Option<u32>,
    pub dataless: bool,
}

impl WalkEntry {
    pub(super) fn from_metadata(
        relative: RootRelativePath,
        metadata: &cap_primitives::fs::Metadata,
        dataless: bool,
    ) -> Self {
        let kind = kind_from_metadata(metadata);
        Self {
            relative,
            kind,
            size: metadata.len(),
            mtime_ms: metadata_mtime_ms(metadata),
            file_id: metadata_file_id(metadata),
            mode: metadata_mode(metadata),
            dataless,
        }
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct WalkStats {
    pub excluded_dirs: u64,
    pub excluded_files: u64,
    pub walk_errors: u64,
    pub walk_err_samples: Vec<String>,
}

impl WalkStats {
    pub(super) fn note_error(&mut self, sample: String) {
        self.walk_errors += 1;
        if self.walk_err_samples.len() < 5 {
            self.walk_err_samples.push(sample);
        }
    }

    pub(super) fn note_invalid_name(&mut self, relative: &Path) {
        self.note_error(format!(
            "{}: name is not valid Unicode on this platform — skipped rather than recorded under a substituted spelling",
            relative.to_string_lossy()
        ));
    }
}

#[cfg(unix)]
fn metadata_file_id(metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    use cap_primitives::fs::MetadataExt;
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn metadata_file_id(_metadata: &cap_primitives::fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
fn metadata_mode(metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    use cap_primitives::fs::MetadataExt;
    Some(metadata.mode() & 0o7777)
}

#[cfg(not(unix))]
fn metadata_mode(_metadata: &cap_primitives::fs::Metadata) -> Option<u32> {
    None
}

fn metadata_mtime_ms(metadata: &cap_primitives::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.into_std().duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn kind_from_metadata(metadata: &cap_primitives::fs::Metadata) -> WalkKind {
    if metadata.is_dir() {
        WalkKind::Dir
    } else if metadata.is_symlink() {
        WalkKind::Symlink
    } else {
        WalkKind::File
    }
}

pub(super) fn walk<C, V>(
    root: &LocalRoot,
    filter: &PathFilter,
    mut checkpoint: C,
    mut visit: V,
) -> std::io::Result<WalkStats>
where
    C: FnMut() -> std::io::Result<()>,
    V: FnMut(WalkEntry),
{
    let mut stats = WalkStats::default();
    let mut directories = vec![RootRelativeDir::new("").expect("the root directory is valid")];

    while let Some(directory) = directories.pop() {
        checkpoint()?;
        let mut children = match root.read_directory(&directory) {
            Ok(children) => children,
            Err(error) if directory.as_str().is_empty() => {
                return Err(crate::pipeline::scan::root_unreadable_error(
                    root.display_path(),
                    error,
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                stats.note_error(format!("{}: {error}", directory.as_str()));
                continue;
            }
            Err(error) => {
                return Err(subtree_error(root, directory.as_str(), error));
            }
        };
        children.sort_by(|left, right| left.name.cmp(&right.name));

        for child in children {
            checkpoint()?;
            let relative = child_path(&directory, &child.name);
            let relative_text = relative.as_str();
            let kind = kind_from_metadata(&child.metadata);
            let keep = match kind {
                WalkKind::Dir => {
                    let (pass, child_might_match) = filter.pass_dir(relative_text);
                    let keep = pass || child_might_match;
                    if !keep {
                        stats.excluded_dirs += 1;
                    }
                    keep
                }
                WalkKind::File | WalkKind::Symlink => {
                    let keep = filter.pass_file(relative_text);
                    if !keep {
                        stats.excluded_files += 1;
                    }
                    keep
                }
            };
            if !keep {
                continue;
            }

            #[cfg(target_os = "macos")]
            let dataless = if kind == WalkKind::File {
                root.is_dataless_file(&relative)
                    .map_err(|error| subtree_error(root, relative.as_str(), error))?
            } else {
                false
            };
            #[cfg(not(target_os = "macos"))]
            let dataless = false;

            visit(WalkEntry::from_metadata(
                relative.clone(),
                &child.metadata,
                dataless,
            ));
            if kind == WalkKind::Dir {
                directories.push(as_directory(relative));
            }
        }
    }

    Ok(stats)
}
