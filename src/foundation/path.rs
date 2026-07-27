//! String operations on relative paths.
//!
//! Repo-wide rule: rel in the tables is always `/`-separated; it only becomes the native separator when it lands on a real filesystem.

use std::path::{Path, PathBuf};

/// Table rel (`/`-separated) → native separator.
pub fn to_native(rel: &str) -> String {
    if cfg!(windows) { rel.replace('/', "\\") } else { rel.to_string() }
}

/// root + rel → real path.
pub fn join_native(root: &Path, rel: &str) -> PathBuf {
    root.join(to_native(rel))
}

/// Native path → table rel. Returns None when it is not under root.
pub fn to_rel(path: &Path, root: &Path) -> Option<String> {
    let r = path.strip_prefix(root).ok()?;
    Some(r.to_string_lossy().replace('\\', "/"))
}

/// Everything before the last segment, without the trailing `/`. None at top level.
pub fn parent(rel: &str) -> Option<&str> {
    rel.rfind('/').map(|i| &rel[..i])
}

/// The last segment.
pub fn base_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Split into (directory prefix including the trailing `/`, last segment). At top level the prefix is empty.
pub fn split_parent(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(i) => (&rel[..=i], &rel[i + 1..]),
        None => ("", rel),
    }
}

/// Split into (stem, extension including the dot). A hidden file (`.gitignore`) counts entirely as stem, with no extension.
pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Infer the separator from how the root is spelled. For CSV export and frontend path joining — what
/// those get is the far machine's root string, and `cfg!(windows)` describes **this** host, so it guesses wrong.
pub fn sep_of(root: &str) -> char {
    if root.contains('\\') { '\\' } else { '/' }
}

/// Whether a rel of unknown provenance is safe to write to disk.
///
/// For unpacking and other "the plan came from the far side" cases: rejects absolute paths, drive
/// letters, `..` traversal and empty segments.
pub fn is_safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains(':')
        && !rel.split('/').any(|seg| seg == ".." || seg.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_and_base_split_at_the_last_slash() {
        assert_eq!(parent("a/b/c.txt"), Some("a/b"));
        assert_eq!(parent("top.txt"), None);
        assert_eq!(base_name("a/b/c.txt"), "c.txt");
        assert_eq!(base_name("top.txt"), "top.txt");
        assert_eq!(split_parent("a/b/c.txt"), ("a/b/", "c.txt"));
        assert_eq!(split_parent("top.txt"), ("", "top.txt"));
    }

    #[test]
    fn hidden_files_have_no_extension() {
        assert_eq!(split_ext("report.pdf"), ("report", ".pdf"));
        assert_eq!(split_ext("archive.tar.gz"), ("archive.tar", ".gz"));
        assert_eq!(split_ext(".gitignore"), (".gitignore", ""));
        assert_eq!(split_ext("README"), ("README", ""));
    }

    #[test]
    fn unsafe_rels_are_refused() {
        assert!(is_safe_rel("a/b.txt"));
        assert!(!is_safe_rel(""), "empty rel");
        assert!(!is_safe_rel("/etc/passwd"), "absolute path");
        assert!(!is_safe_rel("C:/x"), "drive letter");
        assert!(!is_safe_rel("a/../../etc"), "traversal");
        assert!(!is_safe_rel("a//b"), "empty segment");
        assert!(!is_safe_rel(".."));
    }

    #[test]
    fn sep_is_inferred_from_the_root_string_not_the_host() {
        // Exactly the case cfg!(windows) gets backwards: this host is Windows, but the far root is posix
        assert_eq!(sep_of(r"D:\Code\x"), '\\');
        assert_eq!(sep_of("/Users/x/Code"), '/');
    }

    #[test]
    fn rel_roundtrips_through_native() {
        let root = Path::new(if cfg!(windows) { r"D:\R" } else { "/r" });
        let joined = join_native(root, "a/b/c.txt");
        assert_eq!(to_rel(&joined, root).as_deref(), Some("a/b/c.txt"));
    }

    #[test]
    fn to_rel_refuses_paths_outside_root() {
        let root = Path::new(if cfg!(windows) { r"D:\R" } else { "/r" });
        let outside = Path::new(if cfg!(windows) { r"D:\Other\x" } else { "/other/x" });
        assert_eq!(to_rel(outside, root), None);
    }
}
