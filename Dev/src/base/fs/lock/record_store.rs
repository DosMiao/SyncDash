//! Reading and publishing ledger records through a `Vfs`.
//!
//! The only place lock state touches a backend. Every write is a no-replace commit: a claim that
//! loses the race fails rather than overwriting the winner, which is what makes contention safe
//! without any locking primitive from the filesystem.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::Read;
use std::sync::Arc;

use crate::foundation::names::LOCK_NAME;
use crate::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use crate::fs::vfs::{Vfs, WriteHint};

use super::artifact::{invalid_lock, validate_anchor, LockAnchor, LOCK_PROTOCOL};

/// A ledger record is a few hundred bytes. The cap is what stops a corrupt or hostile file from
/// being read into memory in full before it is rejected.
const MAX_LOCK_BYTES: u64 = 16 * 1024;

pub(super) fn read_record<T: DeserializeOwned>(
    vfs: &Arc<dyn Vfs>,
    path: &str,
) -> std::io::Result<Option<T>> {
    let reader = match vfs.open_read(path) {
        Ok(reader) => reader,
        Err(error) if error.kind == VfsErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    reader.take(MAX_LOCK_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOCK_BYTES {
        return Err(invalid_lock(format!(
            "{path} exceeds the {MAX_LOCK_BYTES}-byte lock-record limit"
        )));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| invalid_lock(format!("cannot decode lock record {path:?}: {error}")))
}

pub(super) fn publish_record<T: Serialize>(
    vfs: &Arc<dyn Vfs>,
    path: &str,
    value: &T,
) -> VfsResult<()> {
    let body = serde_json::to_vec(value).map_err(|error| {
        VfsError::new(
            VfsErrorKind::Io,
            format!("could not encode lock record {path:?}: {error}"),
        )
    })?;
    let mut staged = vfs.open_write(path, &WriteHint::default())?;
    staged.write(&body)?;
    staged.seal(false)?;
    staged.commit_noreplace()?;
    Ok(())
}

pub(super) fn read_anchor(vfs: &Arc<dyn Vfs>) -> std::io::Result<Option<LockAnchor>> {
    let anchor = read_record::<LockAnchor>(vfs, LOCK_NAME)?;
    if let Some(anchor) = &anchor {
        validate_anchor(anchor)?;
    }
    Ok(anchor)
}

pub(super) fn ensure_anchor(vfs: &Arc<dyn Vfs>) -> std::io::Result<LockAnchor> {
    if let Some(anchor) = read_anchor(vfs)? {
        return Ok(anchor);
    }
    let has_orphaned_ledger = vfs
        .read_dir_names("")
        .map_err(std::io::Error::from)?
        .into_iter()
        .any(|(name, _)| name.as_str().starts_with(&format!("{LOCK_NAME}.")));
    if has_orphaned_ledger {
        return Err(invalid_lock(format!(
            "{LOCK_NAME} is missing while lock-ledger artifacts remain; refuse to create a new ledger until every owner has stopped and the complete {LOCK_NAME}* namespace is removed"
        )));
    }
    if let Some(anchor) = read_anchor(vfs)? {
        return Ok(anchor);
    }
    let candidate = LockAnchor {
        protocol: LOCK_PROTOCOL,
        ledger_id: crate::fs::vfs::random_name_token().map_err(std::io::Error::from)?,
    };
    match publish_record(vfs, LOCK_NAME, &candidate) {
        Ok(()) => Ok(candidate),
        Err(publish_error) => match read_anchor(vfs)? {
            Some(winner) => Ok(winner),
            None => Err(publish_error.into()),
        },
    }
}
