//! The `.syncdash-root` mount-point marker: written into a root, read back to prove it is mounted.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::foundation::names::MARKER_NAME;
use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Marker {
    pub job: String,
    pub host: String,
    pub created_at_ms: u64,
    /// Free-form note, human-readable
    #[serde(default)]
    pub note: String,
}

pub fn marker_path(root: &Path) -> PathBuf {
    root.join(MARKER_NAME)
}

pub fn read_marker(root: &Path) -> Option<Marker> {
    let root = LocalRoot::open(root.to_path_buf()).ok()?;
    inspect_marker_in(&root).ok().flatten()
}

/// Write the marker (`syncdash mark`). If one already exists, keep its content and report it — never overwrite.
pub fn write_marker(root: &Path, job: &str, note: &str) -> std::io::Result<(PathBuf, bool)> {
    let p = marker_path(root);
    let root = LocalRoot::open(root.to_path_buf())?;
    let relative = marker_relative();
    if inspect_marker_in(&root)?.is_some() {
        return Ok((p, false));
    }
    let m = Marker {
        job: job.to_string(),
        host: crate::model::table::host_name(),
        created_at_ms: crate::foundation::time::now_ms(),
        note: note.to_string(),
    };
    let body = format!("{}\n", serde_json::to_string_pretty(&m)?);
    let mut staged = root.create_staged(&relative)?;
    std::io::Write::write_all(&mut staged, body.as_bytes())?;
    staged.seal(true)?;
    match staged.commit_noreplace() {
        Ok(()) => Ok((p, true)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match inspect_marker_in(&root) {
                Ok(Some(_)) => Ok((p, false)),
                Ok(None) | Err(_) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) fn inspect_marker_in(root: &LocalRoot) -> std::io::Result<Option<Marker>> {
    let relative = marker_relative();
    match root.metadata_path(&relative) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(invalid_marker_type(&marker_path(root.display_path()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let text = root.read_to_string(&relative)?;
    serde_json::from_str(&text).map(Some).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "invalid marker {}: {error}",
                marker_path(root.display_path()).display()
            ),
        )
    })
}

fn marker_relative() -> RootRelativePath {
    RootRelativePath::try_from(MARKER_NAME).expect("the marker constant is one portable entry name")
}

fn invalid_marker_type(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "marker path is occupied by a non-file entry: {}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("syncdash-marker-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn concurrent_markers_publish_exactly_one_record() {
        let root = std::sync::Arc::new(test_root("concurrent"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|job| {
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    write_marker(root.as_path(), job, "").unwrap().1
                })
            })
            .collect();
        let created = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|created| *created)
            .count();

        assert_eq!(created, 1);
        assert!(matches!(
            read_marker(root.as_path()).map(|marker| marker.job),
            Some(job) if job == "first" || job == "second"
        ));
        let _ = std::fs::remove_dir_all(root.as_path());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_redirect_marker_creation() {
        use std::os::unix::fs::symlink;

        let root = test_root("symlink");
        let outside =
            root.with_file_name(format!("syncdash-marker-outside-{}", std::process::id()));
        let _ = std::fs::remove_file(&outside);
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, marker_path(&root)).unwrap();

        assert_eq!(
            write_marker(&root, "job", "").unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }
}
