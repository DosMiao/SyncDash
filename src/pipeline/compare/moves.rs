//! Move detection: a delete and a copy of byte-identical content are one rename.
//!
//! This is the FreeFileSync complaint the tool was built around — a renamed directory should not
//! re-transfer its whole contents. Pairing is by content hash, with a same-parent rename preferred
//! so the reason reads as what actually happened.

use std::collections::{HashMap, HashSet};

use crate::model::table::Entry;
use super::is_conflict_copy;

/// The result of one move pairing
pub struct MovePair {
    pub from: String,
    pub to: String,
    pub size: u64,
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
pub(super) fn detect_moves<'a>(
    adds: Vec<&'a Entry>,
    dels: Vec<&'a Entry>,
) -> (Vec<MovePair>, Vec<&'a Entry>, Vec<&'a Entry>) {
    fn parent(p: &str) -> &str {
        p.rfind('/').map(|i| &p[..i]).unwrap_or("")
    }
    let eligible = |e: &Entry| e.size > 0 && e.hash.is_some() && !is_conflict_copy(&e.path);
    let mut by_key: HashMap<(String, u64), Vec<&'a Entry>> = HashMap::new();
    for &d in &dels {
        if eligible(d) {
            by_key.entry((d.hash.clone().unwrap(), d.size)).or_default().push(d);
        }
    }
    let mut moves = Vec::new();
    let mut rest_adds = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    for a in adds {
        let mut matched = None;
        if eligible(a) {
            if let Some(cands) = by_key.get_mut(&(a.hash.clone().unwrap(), a.size)) {
                if !cands.is_empty() {
                    let n = cands.len();
                    let pick = cands
                        .iter()
                        .position(|c| parent(&c.path) == parent(&a.path))
                        .or_else(|| {
                            cands.iter().position(|c| {
                                std::path::Path::new(&c.path).file_name()
                                    == std::path::Path::new(&a.path).file_name()
                            })
                        })
                        .unwrap_or(0);
                    let c = cands.remove(pick);
                    used.insert(c.path.clone());
                    matched = Some(MovePair {
                        rename_in_place: parent(&c.path) == parent(&a.path),
                        from: c.path.clone(),
                        to: a.path.clone(),
                        size: a.size,
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
    let rest_dels = dels.into_iter().filter(|d| !used.contains(&d.path)).collect();
    (moves, rest_adds, rest_dels)
}

/// The reason for a move op: an ambiguous pairing states the candidate count honestly
pub(super) fn move_reason(base: &str, m: &MovePair) -> String {
    if m.candidates > 1 {
        format!("{base} (ambiguous: {} identical candidates)", m.candidates)
    } else {
        base.to_string()
    }
}
