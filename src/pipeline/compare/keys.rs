//! How two entries are judged the same file, and how a path becomes a comparison key.
//!
//! The key folding is the cross-platform part: macOS writes NFD, Windows NFC, and the same file
//! typed on either machine has to land in the same bucket or every path looks like a rename.

use std::collections::BTreeMap;

use crate::foundation::text::norm_key;
use crate::model::table::{Entry, EntryKind, Snapshot};

pub(super) fn files_equal(a: &Entry, b: &Entry, win_ms: i64) -> bool {
    // Evidence that was asked for and not obtained must never resolve to "equal". The size+mtime
    // line below is the *intended* judgment when hashing was not requested; reaching it because a
    // read failed is a silent downgrade, and it is the one direction that loses data — a file whose
    // content changed under a preserved size and mtime would be declared identical forever.
    // Answering "not equal" here is the safe half; the decision sites turn it into a Conflict so it
    // is not merely re-copied every run.
    if a.hash_failed || b.hash_failed {
        return false;
    }
    if let (Some(ha), Some(hb)) = (&a.hash, &b.hash) {
        return ha == hb;
    }
    a.size == b.size && (a.mtime_ms - b.mtime_ms).abs() <= win_ms
}

/// Whether this pair cannot be judged on content because one side's content could not be read.
pub(super) fn evidence_missing(a: &Entry, b: &Entry) -> bool {
    a.hash_failed || b.hash_failed
}

/// Which generation of archive entry `r` the content of `e` corresponds to:
/// `Some(0)` = matches what the archive currently records, `Some(n)` = the n-th historic generation, `None` = the archive has never seen it.
/// The lower the generation number the newer it is — this is what lets "one generation behind" be told apart from "concurrent edit" (P1-3).
pub(super) fn generation_of(e: &Entry, r: &Entry, win_ms: i64) -> Option<usize> {
    if files_equal(e, r, win_ms) {
        return Some(0);
    }
    let h = e.hash.as_deref()?;
    r.prev.as_ref()?.iter().position(|x| x == h).map(|i| i + 1)
}

/// Normalized key → entry; on a collision (NFD/NFC or case twins) the first one seen is kept and recorded
pub(super) fn map_of<'a>(
    snap: &'a Snapshot,
    kind: EntryKind,
    ci: bool,
) -> (BTreeMap<String, &'a Entry>, Vec<String>) {
    let mut m: BTreeMap<String, &Entry> = BTreeMap::new();
    let mut dups = Vec::new();
    for e in snap.entries.iter().filter(|e| e.kind == kind) {
        let k = norm_key(&e.path, ci);
        if m.contains_key(&k) {
            dups.push(e.path.clone());
        } else {
            m.insert(k, e);
        }
    }
    (m, dups)
}

// Evidence layer (read-only, for the UI; compare() is unaffected)
