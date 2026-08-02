//! Descriptor-relative text reads shared by migration, queries, and retention.

use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;

pub(super) fn read_optional_text(
    root: &LocalRoot,
    relative: &RootRelativePath,
) -> std::io::Result<Option<String>> {
    match root.read_to_string(relative) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
