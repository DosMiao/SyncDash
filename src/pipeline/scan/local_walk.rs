//! Metadata records shared by the platform directory walkers.
//!
//! The scanner consumes one shape regardless of how a directory was enumerated. macOS fills it
//! from `getattrlistbulk`; every other platform, and the macOS differential tests, fill it from
//! WalkDir. Keeping the policy-free record here leaves filtering, hashing, cache use, and snapshot
//! construction in `local` instead of growing a second scanner beside it.

use std::path::{Path, PathBuf};

use crate::pipeline::filter::PathFilter;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum WalkKind {
    Dir,
    File,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WalkEntry {
    pub abs: PathBuf,
    pub rel: String,
    pub kind: WalkKind,
    pub size: u64,
    pub mtime_ms: i64,
    pub file_id: Option<String>,
    pub mode: Option<u32>,
    pub dataless: bool,
}

impl WalkEntry {
    pub(super) fn from_metadata(
        abs: PathBuf,
        rel: String,
        kind: WalkKind,
        md: &std::fs::Metadata,
    ) -> Self {
        Self {
            abs,
            rel,
            kind,
            size: md.len(),
            mtime_ms: metadata_mtime_ms(md),
            file_id: metadata_file_id(md),
            mode: metadata_mode(md),
            dataless: metadata_is_dataless(md),
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

    pub(super) fn note_invalid_name(&mut self, rel: &Path) {
        self.note_error(format!(
            "{}: name is not valid Unicode on this platform — skipped rather than recorded under a substituted spelling",
            rel.to_string_lossy()
        ));
    }
}

#[cfg(unix)]
fn metadata_file_id(md: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("{}:{}", md.dev(), md.ino()))
}

#[cfg(not(unix))]
fn metadata_file_id(_md: &std::fs::Metadata) -> Option<String> {
    None
}

#[cfg(unix)]
fn metadata_mode(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(md.mode() & 0o7777)
}

#[cfg(not(unix))]
fn metadata_mode(_md: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(target_os = "macos")]
fn metadata_is_dataless(md: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    md.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn metadata_is_dataless(_md: &std::fs::Metadata) -> bool {
    false
}

fn metadata_mtime_ms(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn walk<C, V>(
    root: &Path,
    filter: &PathFilter,
    mut checkpoint: C,
    mut visit: V,
) -> std::io::Result<WalkStats>
where
    C: FnMut() -> std::io::Result<()>,
    V: FnMut(WalkEntry),
{
    let excluded_dirs = std::cell::Cell::new(0u64);
    let excluded_files = std::cell::Cell::new(0u64);
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let rel = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            if entry.file_type().is_dir() {
                let (pass, child_might_match) = filter.pass_dir(&rel);
                let keep = pass || child_might_match;
                if !keep {
                    excluded_dirs.set(excluded_dirs.get() + 1);
                }
                keep
            } else {
                let keep = filter.pass_file(&rel);
                if !keep {
                    excluded_files.set(excluded_files.get() + 1);
                }
                keep
            }
        });

    let mut stats = WalkStats::default();
    for item in walker {
        checkpoint()?;
        let item = match item {
            Ok(item) => item,
            Err(error) if error.depth() == 0 => {
                return Err(std::io::Error::other(format!(
                    "scan of '{}' could not read the root itself: {error} — refusing to report it as an empty tree (that reads as a mass deletion on the other side)",
                    root.display()
                )));
            }
            Err(error) => {
                let kind = error.io_error().map(std::io::Error::kind);
                if kind != Some(std::io::ErrorKind::NotFound) {
                    return Err(std::io::Error::new(
                        kind.unwrap_or(std::io::ErrorKind::Other),
                        format!(
                            "scan of '{}' aborted at '{}': {error} — refusing to emit a half table (its missing subtrees would read as deletions)",
                            root.display(),
                            error
                                .path()
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| "<path unavailable>".into()),
                        ),
                    ));
                }
                stats.note_error(format!(
                    "{}: {error}",
                    error
                        .path()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "<path unavailable>".into())
                ));
                continue;
            }
        };
        if item.depth() == 0 {
            continue;
        }

        let raw_rel = item.path().strip_prefix(root).unwrap_or(item.path());
        let Some(rel) = raw_rel.to_str() else {
            stats.note_invalid_name(raw_rel);
            continue;
        };
        let rel = rel.replace('\\', "/");
        let md = match item.metadata() {
            Ok(md) => md,
            Err(error) => {
                stats.note_error(format!("{rel}: {error}"));
                continue;
            }
        };
        let kind = if item.file_type().is_dir() {
            WalkKind::Dir
        } else if item.file_type().is_symlink() {
            WalkKind::Symlink
        } else {
            WalkKind::File
        };
        visit(WalkEntry::from_metadata(
            item.path().to_path_buf(),
            rel,
            kind,
            &md,
        ));
    }
    stats.excluded_dirs = excluded_dirs.get();
    stats.excluded_files = excluded_files.get();
    Ok(stats)
}
