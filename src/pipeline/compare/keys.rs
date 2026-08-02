//! How two entries are judged the same file, and how a path becomes a comparison key.
//!
//! The key folding is the cross-platform part: macOS writes NFD, Windows NFC, and the same file
//! typed on either machine has to land in the same bucket or every path looks like a rename.

use std::collections::BTreeMap;

use crate::foundation::text::norm_key;
use crate::model::table::{
    FileIdentityObservation, ObservedEntry, ObservedEntryKind, TableArtifact,
};

pub(super) fn files_equal(a: &ObservedEntry, b: &ObservedEntry, win_ms: i64) -> bool {
    let (Some(a), Some(b)) = (a.as_file(), b.as_file()) else {
        return false;
    };
    // Evidence that was asked for and not obtained must never resolve to "equal". The size+mtime
    // line below is the *intended* judgment when hashing was not requested; reaching it because a
    // read failed is a silent downgrade, and it is the one direction that loses data — a file whose
    // content changed under a preserved size and mtime would be declared identical forever.
    // Answering "not equal" here is the safe half; the decision sites turn it into a Conflict so it
    // is not merely re-copied every run.
    if a.identity.is_unreadable() || b.identity.is_unreadable() {
        return false;
    }
    match (&a.identity, &b.identity) {
        (FileIdentityObservation::SizeAndMtime, FileIdentityObservation::SizeAndMtime) => {
            a.size == b.size && (a.mtime_ms - b.mtime_ms).abs() <= win_ms
        }
        (left, right) if left.digest().is_some() && right.digest().is_some() => left == right,
        _ => false,
    }
}

/// Whether this pair cannot be judged on content because one side's content could not be read.
pub(super) fn evidence_missing(a: &ObservedEntry, b: &ObservedEntry) -> bool {
    a.as_file()
        .is_some_and(|file| file.identity.is_unreadable())
        || b.as_file()
            .is_some_and(|file| file.identity.is_unreadable())
}

/// Which generation of archive entry `r` the content of `e` corresponds to:
/// `Some(0)` = matches what the archive currently records, `Some(n)` = the n-th historic generation, `None` = the archive has never seen it.
/// Lower generation numbers are newer, distinguishing a lagging side from a concurrent edit.
pub(super) fn generation_of(e: &ObservedEntry, r: &ObservedEntry, win_ms: i64) -> Option<usize> {
    if files_equal(e, r, win_ms) {
        return Some(0);
    }
    let identity = &e.as_file()?.identity;
    r.as_file()?
        .previous_identities
        .iter()
        .position(|previous| previous == identity)
        .map(|index| index + 1)
}

/// Normalized key → entry; on a collision (NFD/NFC or case twins) the first one seen is kept and recorded
pub(super) fn map_of<'a>(
    snap: &'a TableArtifact,
    kind: ObservedEntryKind,
    ci: bool,
) -> (BTreeMap<String, &'a ObservedEntry>, Vec<String>) {
    let mut m: BTreeMap<String, &ObservedEntry> = BTreeMap::new();
    let mut dups = Vec::new();
    for e in snap.entries.iter().filter(|entry| entry.kind() == kind) {
        let k = norm_key(e.path().as_str(), ci);
        if m.contains_key(&k) {
            dups.push(e.path().as_str().to_owned());
        } else {
            m.insert(k, e);
        }
    }
    (m, dups)
}

// Evidence layer (read-only, for the UI; compare() is unaffected)
