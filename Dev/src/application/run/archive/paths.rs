//! Where a destination's archive file lives.

use std::io::{self};
use std::path::Path;

use crate::foundation::path::RootRelativePath;

pub(super) fn archive_location(destination: &Path) -> io::Result<(&Path, RootRelativePath)> {
    let parent = destination
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "archive path has no UTF-8 file name",
            )
        })?;
    let relative = RootRelativePath::try_from(name)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    Ok((parent, relative))
}
