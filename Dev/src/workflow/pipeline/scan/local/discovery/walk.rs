//! Descriptor-relative metadata traversal for the local scanner.

use std::path::Path;

use crate::foundation::path::{RootRelativeDir, RootRelativePath};
use crate::fs::local_root::LocalRoot;
use crate::pipeline::filter::PathFilter;

use super::{as_directory, child_path, directory_path, out_of_scope};

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
            file_id: crate::fs::meta::capability_file_id(metadata),
            mode: crate::fs::meta::capability_unix_mode(metadata),
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
    pub unread_paths: Vec<RootRelativePath>,
}

impl WalkStats {
    pub(super) fn note_error(&mut self, sample: String) {
        self.walk_errors += 1;
        if self.walk_err_samples.len() < 5 {
            self.walk_err_samples.push(sample);
        }
    }

    /// Record a path whose content this process is not allowed to observe, and say why in the log.
    ///
    /// Deliberately not a walk error. That channel means "an entry that was listed is now gone",
    /// which compare is right to read as a deletion. This is the opposite claim — the content may
    /// well still be there, this process just cannot see it — so compare suppresses the path on
    /// both sides instead. The table carries the path because that is what suppression matches on;
    /// the OS error goes to the log, where a reader chasing *why* will be looking.
    pub(super) fn note_unread(&mut self, relative: RootRelativePath, error: &std::io::Error) {
        crate::log_warn!(
            "scan",
            "cannot read '{}': {error} — leaving it out of the comparison on both sides",
            relative.as_str()
        );
        self.unread_paths.push(relative);
    }

    /// A name the platform returned but Unicode cannot spell, on any lane: it is counted into the
    /// walk-error channel and skipped, never recorded under a substituted spelling.
    pub(super) fn note_invalid_name(&mut self, relative: &Path) {
        self.note_error(format!(
            "{}: name is not valid Unicode on this platform — skipped rather than recorded under a substituted spelling",
            relative.to_string_lossy()
        ));
    }
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
        let listing = match root.read_directory_partial(&directory) {
            Ok(listing) => listing,
            // An unreadable root is not a subtree that can be left out: there is no evidence at
            // all, and every entry on the other side would read as deleted.
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
                stats.note_unread(directory_path(&directory), &error);
                continue;
            }
        };
        for name in &listing.invalid_names {
            stats.note_invalid_name(Path::new(name));
        }
        for child in listing.unreadable {
            let relative = child_path(&directory, &child.name);
            // An excluded path this process cannot stat is nobody's missing evidence. Checking the
            // filter first is what makes excluding such a path actually work — the alternative
            // reports a path the user has already said is none of this job's business.
            if out_of_scope(filter, relative.as_str()) {
                continue;
            }
            stats.note_unread(relative, &child.error);
        }
        let mut children = listing.entries;
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

            // A file whose cloud-placeholder state cannot be probed is a file whose bytes may or
            // may not be on this disk. Recording it either way is a guess, so it joins the paths
            // left out of the comparison instead.
            let dataless = if kind == WalkKind::File {
                match root.is_dataless_file(&relative, &child.metadata) {
                    Ok(dataless) => dataless,
                    Err(error) => {
                        stats.note_unread(relative, &error);
                        continue;
                    }
                }
            } else {
                false
            };

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
