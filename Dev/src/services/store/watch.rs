//! Crash-safe persistence for filesystem-watcher cursors.
//!
//! The persisted position is the native watcher cursor itself, including journal UUID, epoch, and
//! event ID for every root.  A cursor is useful only when every stream still belongs to the same
//! watcher journal.  The loader therefore distinguishes a missing checkpoint from an invalid one;
//! callers must turn an invalid checkpoint into a full-tree bootstrap instead of silently treating
//! it as replayable.
//! Writes use the same staged, fsynced, same-directory rename as the scanner caches, so a crash
//! leaves either the previous complete generation or the new complete generation on disk.

use std::path::{Path, PathBuf};

use crate::fs::watch::WatchPosition;

const CHECKPOINT_SCHEMA: u32 = 1;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct StoredCheckpoint {
    schema: u32,
    owner_binding: String,
    position: WatchPosition,
}

/// Result of reading durable watcher state.
///
/// `Invalid` is deliberately not folded into `Missing`: corruption, a schema mismatch, and a job
/// identity mismatch all require an explicit full-scan fallback and should be observable by the
/// orchestration layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointLoad {
    Missing,
    Valid(WatchPosition),
    Invalid(String),
}

/// Durable state for one compare job.
#[derive(Clone, Debug)]
pub struct CheckpointStore {
    owner: String,
    path: PathBuf,
}

impl CheckpointStore {
    /// Use the normal per-user SyncDash state directory.
    ///
    /// `owner` should be a stable job/configuration identity that changes when either watched root
    /// changes.  Only its digest is written to disk, so a root phrase containing credentials is not
    /// copied into the checkpoint.
    pub fn for_job(owner: impl Into<String>) -> Self {
        let owner = owner.into();
        let hash = blake3::hash(owner.to_lowercase().as_bytes());
        let path = crate::foundation::dirs::data_dir()
            .join("watch")
            .join(format!("{}.json", &hash.to_hex()[..16]));
        Self { owner, path }
    }

    /// Construct a store at a caller-owned path.  This is useful for portable runners and tests;
    /// the checkpoint still embeds and validates `owner`.
    pub fn at_path(owner: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            owner: owner.into(),
            path: path.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> std::io::Result<CheckpointLoad> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CheckpointLoad::Missing);
            }
            Err(error) => return Err(error),
        };
        let stored: StoredCheckpoint = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            Err(error) => {
                return Ok(CheckpointLoad::Invalid(format!(
                    "invalid checkpoint JSON: {error}"
                )))
            }
        };
        if stored.schema != CHECKPOINT_SCHEMA {
            return Ok(CheckpointLoad::Invalid(format!(
                "watch checkpoint schema {} is not supported (expected {CHECKPOINT_SCHEMA})",
                stored.schema
            )));
        }
        if stored.owner_binding != owner_binding(&self.owner) {
            return Ok(CheckpointLoad::Invalid(format!(
                "watch checkpoint owner binding does not match the active job"
            )));
        }
        if let Err(reason) = stored.position.validate() {
            return Ok(CheckpointLoad::Invalid(reason));
        }
        Ok(CheckpointLoad::Valid(stored.position))
    }

    /// Atomically publish a cursor after the corresponding compare work has succeeded.
    pub fn save(&self, position: &WatchPosition) -> std::io::Result<()> {
        position
            .validate()
            .map_err(|reason| std::io::Error::new(std::io::ErrorKind::InvalidInput, reason))?;
        let stored = StoredCheckpoint {
            schema: CHECKPOINT_SCHEMA,
            owner_binding: owner_binding(&self.owner),
            position: position.clone(),
        };
        super::atomic::rewrite(&self.path, |writer| {
            serde_json::to_writer(&mut *writer, &stored).map_err(std::io::Error::other)?;
            writer.write_all(b"\n")
        })
    }
}

fn owner_binding(owner: &str) -> String {
    format!("blake3:{}", blake3::hash(owner.as_bytes()).to_hex())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::fs::watch::RootCursor;

    fn temp_store(tag: &str) -> (PathBuf, CheckpointStore) {
        let directory = std::env::temp_dir().join(format!(
            "syncdash-watch-checkpoint-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let store = CheckpointStore::at_path("job-a", directory.join("checkpoint.json"));
        (directory, store)
    }

    fn position(event_id: u64) -> WatchPosition {
        WatchPosition {
            streams: BTreeMap::from([
                (
                    "source".into(),
                    RootCursor {
                        journal_uuid: Some("source-journal-uuid".into()),
                        epoch: "fsevents:source-journal-uuid".into(),
                        event_id,
                    },
                ),
                (
                    "target".into(),
                    RootCursor {
                        journal_uuid: None,
                        epoch: "fsevents:volatile:target".into(),
                        event_id: event_id + 10,
                    },
                ),
            ]),
        }
    }

    #[test]
    fn missing_and_valid_checkpoints_are_distinct() {
        let (directory, store) = temp_store("roundtrip");
        assert_eq!(store.load().unwrap(), CheckpointLoad::Missing);

        let expected = position(42);
        store.save(&expected).unwrap();
        assert_eq!(store.load().unwrap(), CheckpointLoad::Valid(expected));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn checkpoint_persists_the_exact_native_cursor_shape() {
        let (directory, store) = temp_store("native-shape");
        let expected = position(42);
        store.save(&expected).unwrap();

        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(
            json["position"]["streams"]["source"]["journal_uuid"],
            "source-journal-uuid"
        );
        assert_eq!(
            json["position"]["streams"]["source"]["epoch"],
            "fsevents:source-journal-uuid"
        );
        assert_eq!(json["position"]["streams"]["source"]["event_id"], 42);
        assert!(json["position"]["streams"]["source"]
            .get("offset")
            .is_none());
        assert_eq!(store.load().unwrap(), CheckpointLoad::Valid(expected));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn incompatible_generic_offset_cursors_are_rejected() {
        let (directory, store) = temp_store("generic-offset");
        store.save(&position(42)).unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        let source = json["position"]["streams"]["source"]
            .as_object_mut()
            .unwrap();
        let event_id = source.remove("event_id").unwrap();
        source.insert("offset".into(), event_id);
        std::fs::write(store.path(), serde_json::to_vec(&json).unwrap()).unwrap();

        assert!(matches!(store.load().unwrap(), CheckpointLoad::Invalid(_)));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn corrupt_or_wrong_owner_checkpoints_force_an_explicit_fallback() {
        let (directory, store) = temp_store("invalid");
        std::fs::write(store.path(), b"{truncated").unwrap();
        assert!(matches!(store.load().unwrap(), CheckpointLoad::Invalid(_)));

        CheckpointStore::at_path("other-job", store.path())
            .save(&position(7))
            .unwrap();
        let result = store.load().unwrap();
        assert!(
            matches!(result, CheckpointLoad::Invalid(reason) if reason.contains("owner binding"))
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn invalid_positions_are_never_published() {
        let (directory, store) = temp_store("invalid-position");
        let invalid = WatchPosition {
            streams: BTreeMap::new(),
        };
        let error = store.save(&invalid).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(store.load().unwrap(), CheckpointLoad::Missing);
        let _ = std::fs::remove_dir_all(directory);
    }
}
