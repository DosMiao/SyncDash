//! Move detection: a delete and a copy of byte-identical content are one rename.
//!
//! This is the FreeFileSync complaint the tool was built around — a renamed directory should not
//! re-transfer its whole contents. Pairing is by content hash, with a same-parent rename preferred
//! so the reason reads as what actually happened.

use std::collections::{HashMap, HashSet};

use super::super::is_conflict_copy;
use crate::model::table::{Blake3Digest, FileIdentityObservation, ObservedEntry, ObservedFile};

fn full_identity(entry: &ObservedEntry) -> Option<(&ObservedFile, &Blake3Digest)> {
    let file = entry.as_file()?;
    let FileIdentityObservation::FullBlake3 { digest } = &file.identity else {
        return None;
    };
    (file.size > 0 && !is_conflict_copy(file.path.as_str())).then_some((file, digest))
}

/// The result of one move pairing
pub struct MovePair {
    pub from: String,
    pub to: String,
    pub size: u64,
    pub mtime_ms: i64,
    pub hash: String,
    pub mode: Option<u32>,
    /// Same parent dir = rename in place (FFS's "same-directory rename merge")
    pub rename_in_place: bool,
    /// Number of same-content candidates at pairing time. >1 means `from` was picked arbitrarily among equivalent candidates —
    /// the resulting content is still correct, but the attribution is uncertain; reason must say so honestly, never feign certainty.
    pub candidates: usize,
}

/// Move pairing. Pairing priority (in the spirit of FFS's "same-directory rename merge"):
///   1) same parent dir (rename in place)  2) same file name (whole directory relocated)  3) any same hash
///
/// **Empty files never take part in pairing**: every zero-length file has the same blake3, so they all
/// crowd into one bucket and a pile of unrelated `__init__.py` / `.gitkeep` get paired as "renames".
/// The very first thing syncthing does in `findRename` (`lib/model/folder.go:930-932`) is exclude
/// `Size == 0`; we do the same.
pub(in crate::pipeline::compare) fn detect_moves<'a>(
    adds: Vec<&'a ObservedEntry>,
    dels: Vec<&'a ObservedEntry>,
) -> (
    Vec<MovePair>,
    Vec<&'a ObservedEntry>,
    Vec<&'a ObservedEntry>,
) {
    fn parent(p: &str) -> &str {
        p.rfind('/').map(|i| &p[..i]).unwrap_or("")
    }
    let mut by_key: HashMap<(String, u64), Vec<&'a ObservedEntry>> = HashMap::new();
    for &d in &dels {
        if let Some((file, digest)) = full_identity(d) {
            by_key
                .entry((digest.as_str().to_owned(), file.size))
                .or_default()
                .push(d);
        }
    }
    let mut moves = Vec::new();
    let mut rest_adds = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    for a in adds {
        let mut matched = None;
        if let Some((added_file, added_digest)) = full_identity(a) {
            if let Some(cands) =
                by_key.get_mut(&(added_digest.as_str().to_owned(), added_file.size))
            {
                if !cands.is_empty() {
                    let n = cands.len();
                    let pick = cands
                        .iter()
                        .position(|candidate| {
                            parent(candidate.path().as_str()) == parent(added_file.path.as_str())
                        })
                        .or_else(|| {
                            cands.iter().position(|candidate| {
                                std::path::Path::new(candidate.path().as_str()).file_name()
                                    == std::path::Path::new(added_file.path.as_str()).file_name()
                            })
                        })
                        .unwrap_or(0);
                    let c = cands.remove(pick);
                    let deleted_file = c
                        .as_file()
                        .expect("move candidates come from the file observation map");
                    used.insert(deleted_file.path.as_str().to_owned());
                    matched = Some(MovePair {
                        rename_in_place: parent(deleted_file.path.as_str())
                            == parent(added_file.path.as_str()),
                        from: deleted_file.path.as_str().to_owned(),
                        to: added_file.path.as_str().to_owned(),
                        size: deleted_file.size,
                        mtime_ms: deleted_file.mtime_ms,
                        hash: deleted_file
                            .identity
                            .digest()
                            .expect("move candidates carry full BLAKE3 evidence")
                            .as_str()
                            .to_owned(),
                        mode: deleted_file.mode,
                        candidates: n,
                    });
                }
            }
        }
        match matched {
            Some(m) => moves.push(m),
            None => rest_adds.push(a),
        }
    }
    let rest_dels = dels
        .into_iter()
        .filter(|entry| !used.contains(entry.path().as_str()))
        .collect();
    (moves, rest_adds, rest_dels)
}

/// The reason for a move op: an ambiguous pairing states the candidate count honestly
pub(in crate::pipeline::compare) fn move_reason(base: &str, m: &MovePair) -> String {
    if m.candidates > 1 {
        format!("{base} (ambiguous: {} identical candidates)", m.candidates)
    } else {
        base.to_string()
    }
}
