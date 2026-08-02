//! Opening the archive file and holding it while it is replaced.
//!
//! The archive lives beside the destination it describes, reached through a `LocalRoot` so the
//! path cannot escape that directory. The lock is held across read-modify-write because two
//! concurrent applies refreshing the same archive would otherwise interleave and produce a record
//! that matches neither run.

use super::paths::archive_location;
use super::publish::{hash_file, publish_immutable_copy, publish_receipt};
use crate::model::digest::Blake3Digest;
use crate::model::table::TableArtifact;
use std::io::{self, Write};
use std::path::Path;

use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;

pub(super) struct ArchiveTarget {
    pub(super) parent: LocalRoot,
    pub(super) relative: RootRelativePath,
}

pub(super) struct ArchiveLock {
    pub(super) _file: std::fs::File,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ArchiveMigrationReceipt {
    pub(super) schema: String,
    pub(super) version: u32,
    pub(super) archive: RootRelativePath,
    pub(super) backup: RootRelativePath,
    source_blake3: Blake3Digest,
    target_blake3: Blake3Digest,
}

impl ArchiveTarget {
    pub(super) fn open_for_read(destination: &Path) -> io::Result<Option<Self>> {
        let (parent_path, relative) = archive_location(destination)?;
        let parent = match LocalRoot::open(parent_path.to_path_buf()) {
            Ok(parent) => parent,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let target = Self { parent, relative };
        if target.validate_existing(destination)? {
            Ok(Some(target))
        } else {
            Ok(None)
        }
    }

    pub(super) fn open_for_write(destination: &Path) -> io::Result<Self> {
        let (parent_path, relative) = archive_location(destination)?;
        std::fs::create_dir_all(parent_path)?;
        let target = Self {
            parent: LocalRoot::open(parent_path.to_path_buf())?,
            relative,
        };
        target.validate_existing(destination)?;
        Ok(target)
    }

    pub(super) fn validate_existing(&self, destination: &Path) -> io::Result<bool> {
        match self.parent.metadata_path(&self.relative) {
            Ok(metadata) if metadata.is_file() => Ok(true),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "archive path is not a regular file: {}",
                    destination.display()
                ),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(super) fn acquire_lock(&self) -> io::Result<ArchiveLock> {
        let file = self.parent.open_lock_file(&self.migration_path("lock")?)?;
        file.lock()?;
        Ok(ArchiveLock { _file: file })
    }

    pub(super) fn migration_path(&self, suffix: &str) -> io::Result<RootRelativePath> {
        let digest = Blake3Digest::hash_bytes(self.relative.as_str().as_bytes());
        RootRelativePath::try_from(format!(
            ".syncdash.archive-migration.{}.{}",
            &digest.as_str()[..16],
            suffix
        ))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))
    }

    pub(super) fn load_current(&self) -> io::Result<Option<TableArtifact>> {
        match self.parent.open_read(&self.relative) {
            Ok(file) => TableArtifact::read_archive(io::BufReader::new(file)).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn load_or_migrate(&self, _lock: &ArchiveLock) -> io::Result<Option<TableArtifact>> {
        let format = match self.parent.open_read(&self.relative) {
            Ok(file) => crate::model::table::migrate::classify_archive(io::BufReader::new(file))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if format == crate::model::table::migrate::ArchiveFormat::Current {
            return self.load_current();
        }

        let source_blake3 = hash_file(&self.parent, &self.relative)?;
        let migrated = {
            let file = self.parent.open_read(&self.relative)?;
            crate::model::table::migrate::migrate_v1_archive(io::BufReader::new(file))?
        };
        let mut target_bytes = Vec::new();
        migrated.write_to(&mut target_bytes)?;
        let target_blake3 = Blake3Digest::hash_bytes(&target_bytes);
        let backup = self.migration_path("v1.backup")?;
        publish_immutable_copy(&self.parent, &self.relative, &backup, &source_blake3)?;
        let receipt_path = self.migration_path("prepared.json")?;
        let receipt = ArchiveMigrationReceipt {
            schema: "syncdash.archive-migration".into(),
            version: 1,
            archive: self.relative.clone(),
            backup,
            source_blake3,
            target_blake3: target_blake3.clone(),
        };
        publish_receipt(&self.parent, &receipt_path, &receipt)?;
        if hash_file(&self.parent, &self.relative)? != receipt.source_blake3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive changed after its migration was prepared; refusing replacement",
            ));
        }
        let mut staged = self.parent.create_staged(&self.relative)?;
        staged.write_all(&target_bytes)?;
        staged.seal(true)?;
        if let Err(error) = staged.commit() {
            match hash_file(&self.parent, &self.relative) {
                Ok(actual) if actual == receipt.target_blake3 => {}
                _ => return Err(error),
            }
        }
        self.load_current()
    }
}
