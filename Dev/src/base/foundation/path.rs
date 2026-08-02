//! Root-contained path types and separator conversion.
//!
//! Table paths always use `/`. These types prove containment; filename legality remains a
//! property of the destination filesystem and is checked against its observed naming rules.

use serde::{Deserialize, Deserializer, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathBoundaryError {
    value: String,
    boundary: &'static str,
    reason: &'static str,
}

impl fmt::Display for PathBoundaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid {} {:?}: {}",
            self.boundary, self.value, self.reason
        )
    }
}

impl std::error::Error for PathBoundaryError {}

#[derive(Clone, Copy)]
enum RelativeBoundary {
    Path,
    Directory,
    EntryName,
}

impl RelativeBoundary {
    fn label(self) -> &'static str {
        match self {
            Self::Path => "root-relative path",
            Self::Directory => "root-relative directory",
            Self::EntryName => "directory entry name",
        }
    }

    fn allows_root(self) -> bool {
        matches!(self, Self::Directory)
    }
}

fn validate_root_relative(
    value: &str,
    boundary: RelativeBoundary,
) -> Result<(), PathBoundaryError> {
    let fail = |reason| PathBoundaryError {
        value: value.to_owned(),
        boundary: boundary.label(),
        reason,
    };
    if value.is_empty() {
        return if boundary.allows_root() {
            Ok(())
        } else {
            Err(fail("the empty string names the root, not an entry"))
        };
    }
    if value.contains('\0') {
        return Err(fail("NUL is not a valid filename byte"));
    }
    if value.contains('\\') {
        return Err(fail(
            "backslashes are not accepted in portable relative paths",
        ));
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(fail("drive-qualified paths are not allowed"));
    }
    if value.starts_with('/') {
        return Err(fail("absolute paths are not allowed"));
    }
    if matches!(boundary, RelativeBoundary::EntryName) && value.contains('/') {
        return Err(fail("a directory entry must contain exactly one segment"));
    }
    if value
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(fail("segments must be non-empty and cannot be '.' or '..'"));
    }
    Ok(())
}

macro_rules! relative_boundary_type {
    ($name:ident, $boundary:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, PathBoundaryError> {
                let value = value.into();
                validate_root_relative(&value, $boundary)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = PathBoundaryError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = PathBoundaryError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_from(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

relative_boundary_type!(
    RootRelativePath,
    RelativeBoundary::Path,
    "A non-empty, slash-separated path resolved beneath one sync root."
);
relative_boundary_type!(
    RootRelativeDir,
    RelativeBoundary::Directory,
    "A portable directory path beneath one sync root; the empty value denotes the root itself."
);
relative_boundary_type!(
    EntryName,
    RelativeBoundary::EntryName,
    "One portable directory-entry name, never a path with multiple segments."
);

/// Table rel (`/`-separated) → native separator.
pub fn to_native(rel: &str) -> String {
    if cfg!(windows) {
        rel.replace('/', "\\")
    } else {
        rel.to_string()
    }
}

/// Native root + portable relative path → native path.
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
    if root.contains('\\') {
        '\\'
    } else {
        '/'
    }
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
        assert!(RootRelativePath::try_from("a/b.txt").is_ok());
        assert!(RootRelativePath::try_from("").is_err(), "empty rel");
        assert!(
            RootRelativePath::try_from("/etc/passwd").is_err(),
            "absolute path"
        );
        assert!(
            RootRelativePath::try_from("C:/x").is_err(),
            "absolute drive path"
        );
        assert!(
            RootRelativePath::try_from("C:relative").is_err(),
            "drive-relative path"
        );
        assert!(
            RootRelativePath::try_from("report:2024.pdf").is_ok(),
            "a filename allowed by POSIX must reach destination-aware name validation"
        );
        assert!(
            RootRelativePath::try_from("a/../../etc").is_err(),
            "traversal"
        );
        assert!(RootRelativePath::try_from("a//b").is_err(), "empty segment");
        assert!(RootRelativePath::try_from("..").is_err());
    }

    #[test]
    fn backslash_traversal_is_refused() {
        assert!(
            RootRelativePath::try_from(r"..\SIBLING.txt").is_err(),
            "bare backslash traversal"
        );
        assert!(
            RootRelativePath::try_from(r"a\..\..\ESCAPED.txt").is_err(),
            "backslash traversal behind a segment"
        );
        assert!(
            RootRelativePath::try_from(r"a/b\..\..\..\ESC2.txt").is_err(),
            "mixed separators"
        );
        assert!(
            RootRelativePath::try_from(r"\server\share\x").is_err(),
            "leading backslash (UNC-shaped)"
        );
        assert!(
            RootRelativePath::try_from(r"a\.\b").is_err(),
            "single-dot segment"
        );
        assert!(
            RootRelativePath::try_from(r"my\file.txt").is_err(),
            "non-portable separator"
        );
    }

    #[test]
    fn boundary_types_enforce_their_distinct_shapes() {
        assert_eq!(
            RootRelativePath::new("a/b.txt").unwrap().as_str(),
            "a/b.txt"
        );
        assert!(RootRelativePath::new("").is_err());
        assert_eq!(RootRelativeDir::new("").unwrap().as_str(), "");
        assert!(RootRelativeDir::new("../outside").is_err());
        assert_eq!(EntryName::new("photo.jpg").unwrap().as_str(), "photo.jpg");
        assert!(EntryName::new("nested/photo.jpg").is_err());
        assert!(EntryName::new("..").is_err());
    }

    #[test]
    fn deserialization_cannot_bypass_path_validation() {
        assert!(serde_json::from_str::<RootRelativePath>(r#""safe/file""#).is_ok());
        assert!(serde_json::from_str::<RootRelativePath>(r#""../outside""#).is_err());
        assert!(serde_json::from_str::<RootRelativeDir>(r#""""#).is_ok());
        assert!(serde_json::from_str::<EntryName>(r#""two/segments""#).is_err());
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
        let outside = Path::new(if cfg!(windows) {
            r"D:\Other\x"
        } else {
            "/other/x"
        });
        assert_eq!(to_rel(outside, root), None);
    }
}
