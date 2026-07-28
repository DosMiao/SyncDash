//! The `.syncdash-root` mount-point marker: written into a root, read back to prove it is mounted.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::foundation::names::MARKER_NAME;

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

pub fn has_marker(root: &Path) -> bool {
    marker_path(root).is_file()
}

pub fn read_marker(root: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(marker_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write the marker (`syncdash mark`). If one already exists, keep its content and report it — never overwrite.
pub fn write_marker(root: &Path, job: &str, note: &str) -> std::io::Result<(PathBuf, bool)> {
    let p = marker_path(root);
    if p.is_file() {
        return Ok((p, false));
    }
    if !root.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("not a directory: {}", root.display()),
        ));
    }
    let m = Marker {
        job: job.to_string(),
        host: crate::model::table::host_name(),
        created_at_ms: crate::foundation::time::now_ms(),
        note: note.to_string(),
    };
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(&m)?))?;
    Ok((p, true))
}
