//! One acceleration table read and published as a bound generation.
//!
//! `hashcache` and `mtimefix` persist different rows, but reach them through the same skeleton:
//! the same per-key filename derivation, the same all-or-nothing bound read, and the same sorted
//! whole-table rewrite. Only the row codec, the retained value, and the write-side retention
//! policy belong to an individual table, so only those stay in the table's own module.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{binding, location, reporting, ReadStatus};

/// A table's fixed identity: the `kind` written into its header, the extension its files carry,
/// and how one decoded row becomes a table entry.
pub(crate) struct BoundTable<Key, Value, Row> {
    kind: &'static str,
    extension: &'static str,
    insert: fn(&mut HashMap<Key, Value>, Row),
}

impl<Key, Value, Row> BoundTable<Key, Value, Row> {
    pub(crate) const fn new(
        kind: &'static str,
        extension: &'static str,
        insert: fn(&mut HashMap<Key, Value>, Row),
    ) -> Self {
        BoundTable {
            kind,
            extension,
            insert,
        }
    }

    pub(crate) fn logical_file(&self, key: &str) -> PathBuf {
        location::logical_file(key, self.extension)
    }

    pub(crate) fn local_file(&self, key: &str) -> PathBuf {
        location::local_file(key, self.extension)
    }
}

impl<Key, Value, Row: serde::de::DeserializeOwned> BoundTable<Key, Value, Row> {
    /// A generation that is not current and correctly bound contributes nothing: an empty table is
    /// safer than acceleration state of uncertain provenance.
    fn read(
        &self,
        file: &Path,
        binding: &[u8],
    ) -> std::io::Result<(HashMap<Key, Value>, ReadStatus)> {
        let mut map = HashMap::new();
        let status = super::read(file, self.kind, binding, |row: Row| {
            (self.insert)(&mut map, row);
        })?;
        if status != ReadStatus::Accepted {
            map.clear();
        }
        Ok((map, status))
    }

    pub(crate) fn read_best_effort(
        &self,
        file: &Path,
        binding: &[u8],
    ) -> Option<(HashMap<Key, Value>, ReadStatus)> {
        reporting::report_read(file, self.read(file, binding))
    }

    pub(crate) fn load_file(&self, file: &Path, binding: &[u8]) -> HashMap<Key, Value> {
        self.read_best_effort(file, binding)
            .map_or_else(HashMap::new, |(map, _)| map)
    }

    /// `None` means the previous generation could not be read at all. A caller about to merge into
    /// it must fail rather than publish a table that silently drops rows it never saw.
    pub(crate) fn load_logical(&self, key: &str) -> Option<HashMap<Key, Value>> {
        let file = self.logical_file(key);
        self.read_best_effort(&file, &binding::logical_binding(key))
            .map(|(map, _)| map)
    }

    pub(crate) fn load_by_key(&self, key: &str) -> HashMap<Key, Value> {
        self.load_logical(key).unwrap_or_default()
    }

    pub(crate) fn load_local(
        &self,
        identity: &super::super::localid::LocalScanStateIdentity,
    ) -> HashMap<Key, Value> {
        if !identity.persistent_reuse() {
            return HashMap::new();
        }
        self.load_file(&self.local_file(identity.cache_key()), identity.binding())
    }
}

impl<Key: Ord, Value, Row> BoundTable<Key, Value, Row> {
    /// Publish the complete table in key order. A row the table declines to write is simply
    /// dropped from the new generation, which is how retention policies prune.
    pub(crate) fn rewrite(
        &self,
        file: &Path,
        binding: &[u8],
        map: &HashMap<Key, Value>,
        mut write_row: impl FnMut(&mut dyn Write, &Key, &Value) -> std::io::Result<()>,
    ) -> std::io::Result<bool> {
        let mut rows: Vec<_> = map.iter().collect();
        rows.sort_unstable_by(|left, right| left.0.cmp(right.0));
        super::rewrite(file, self.kind, binding, |writer| {
            for (key, value) in rows {
                write_row(writer, key, value)?;
            }
            Ok(())
        })
    }
}
