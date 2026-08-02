//! Replacing an archive without losing the one it replaces.
//!
//! The previous archive is copied aside and a receipt records both digests before the new one is
//! published. An archive is the only record of what the two sides last agreed on: losing it turns
//! the next sync's conflicts into silent overwrites, so a half-finished replacement has to be
//! recognisable afterwards rather than merely unlikely.

use super::target::{ArchiveLock, ArchiveMigrationReceipt, ArchiveTarget};
use crate::model::digest::Blake3Digest;
use std::io::{self, Write};

use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;

pub(super) fn hash_file(root: &LocalRoot, relative: &RootRelativePath) -> io::Result<Blake3Digest> {
    use std::io::Read as _;
    let mut file = root.open_read(relative)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Blake3Digest::from_hash(hasher.finalize()))
}

pub(super) fn publish_immutable_copy(
    root: &LocalRoot,
    source: &RootRelativePath,
    destination: &RootRelativePath,
    expected: &Blake3Digest,
) -> io::Result<()> {
    let mut source_file = root.open_read(source)?;
    let mut staged = root.create_staged(destination)?;
    staged.write_all_from(&mut source_file)?;
    staged.seal(true)?;
    match staged.commit_noreplace() {
        Ok(()) => {}
        Err(_error) if hash_file(root, destination).ok().as_ref() == Some(expected) => {}
        Err(error) => return Err(error),
    }
    let actual = hash_file(root, destination)?;
    if &actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "immutable archive migration backup does not match its source",
        ));
    }
    Ok(())
}

pub(super) fn publish_receipt(
    root: &LocalRoot,
    path: &RootRelativePath,
    expected: &ArchiveMigrationReceipt,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(expected).map_err(io::Error::other)?;
    bytes.push(b'\n');
    let expected_hash = Blake3Digest::hash_bytes(&bytes);
    let mut staged = root.create_staged(path)?;
    staged.write_all(&bytes)?;
    staged.seal(true)?;
    match staged.commit_noreplace() {
        Ok(()) => {}
        Err(_error) if hash_file(root, path).ok().as_ref() == Some(&expected_hash) => {}
        Err(error) => return Err(error),
    }
    let stored: ArchiveMigrationReceipt =
        serde_json::from_slice(&root.read(path)?).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid archive migration receipt: {error}"),
            )
        })?;
    if &stored != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "archive migration receipt does not match the prepared migration",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn write_archive_atomic(
    dst: &std::path::Path,
    write_snapshot: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    before_commit: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let target = ArchiveTarget::open_for_write(dst)?;
    let lock = target.acquire_lock()?;
    write_archive_to(&target, &lock, write_snapshot, before_commit)
}

pub(super) fn write_archive_to(
    target: &ArchiveTarget,
    _lock: &ArchiveLock,
    write_snapshot: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    before_commit: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let mut staged = target.parent.create_staged(&target.relative)?;
    {
        let mut writer = io::BufWriter::new(&mut staged);
        write_snapshot(&mut writer)?;
        writer.flush()?;
    }
    staged.seal(true)?;
    before_commit()?;
    staged.commit()
}
