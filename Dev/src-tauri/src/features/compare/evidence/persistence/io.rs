//! Canonical JSON and staged-file commit primitives.

use std::io::Write;

use serde::Serialize;
use syncdash::foundation::path::RootRelativePath;
use syncdash::fs::local_root::LocalRoot;

use super::error::invalid_data;

pub(super) fn serialize_json(value: &impl Serialize) -> std::io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| invalid_data(format!("cannot encode Compare-result JSON: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(super) fn write_staged(
    root: &LocalRoot,
    relative: &RootRelativePath,
    bytes: &[u8],
    no_replace: bool,
) -> std::io::Result<()> {
    let mut staged = root.create_staged(relative)?;
    staged.write_all(bytes)?;
    staged.seal(true)?;
    if no_replace {
        staged.commit_noreplace()
    } else {
        staged.commit()
    }
}
