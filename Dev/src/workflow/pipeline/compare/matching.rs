//! How two entries are judged the same file, and how a path becomes a comparison key.
//!
//! The key folding is the cross-platform part: macOS writes NFD, Windows NFC, and the same file
//! typed on either machine has to land in the same bucket or every path looks like a rename.

pub(super) mod moves;
pub(super) mod name_rules;

use std::collections::btree_map::Entry;
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

/// The subtrees neither side may be judged inside, folded to comparison keys.
///
/// A scan that could not read a directory records it rather than aborting, which leaves that
/// directory in the table with zero children. Zero children is indistinguishable from "children
/// deleted", so every decision site has to be blind to those paths — on *both* sides, not just the
/// side that could not read them, because the whole risk is the other side's copies being planned
/// away against evidence nobody gathered.
///
/// Suppressing at the key layer rather than at each decision site is deliberate: `map_of` is the
/// one door every comparison walks through, so a new decision site cannot forget to ask.
pub(super) struct UnreadScope {
    prefixes: Vec<String>,
    case_insensitive: bool,
}

impl UnreadScope {
    pub(super) fn of(source: &TableArtifact, target: &TableArtifact, ci: bool) -> Self {
        let mut prefixes: Vec<String> = source
            .header
            .unread_paths
            .iter()
            .chain(target.header.unread_paths.iter())
            .map(|path| norm_key(path.as_str(), ci))
            .collect();
        prefixes.sort();
        prefixes.dedup();
        Self {
            prefixes,
            case_insensitive: ci,
        }
    }

    /// Whether an unread subtree lies strictly *under* `path`.
    ///
    /// The question only a directory deletion has to ask. Every other operation acts on its own
    /// path, so keeping unread paths out of the maps is enough; removing a directory also removes
    /// everything beneath it, so an ancestor deletion would take the subtree nobody was allowed to
    /// look at with it — the exact loss the suppression exists to prevent, arriving one level up.
    pub(super) fn encloses_unread(&self, path: &str) -> bool {
        if self.prefixes.is_empty() {
            return false;
        }
        let key = norm_key(path, self.case_insensitive);
        self.prefixes.iter().any(|prefix| {
            prefix.len() > key.len()
                && prefix.as_bytes()[key.len()] == b'/'
                && prefix.starts_with(key.as_str())
        })
    }

    /// Whether `path` lies at or under an unread subtree. Whole-segment prefixes only, so `docs`
    /// covers `docs/a.txt` and leaves `docs-old/a.txt` alone.
    pub(super) fn covers(&self, path: &str) -> bool {
        if self.prefixes.is_empty() {
            return false;
        }
        let key = norm_key(path, self.case_insensitive);
        self.prefixes.iter().any(|prefix| {
            key == *prefix
                || (key.len() > prefix.len()
                    && key.as_bytes()[prefix.len()] == b'/'
                    && key.starts_with(prefix.as_str()))
        })
    }
}

/// Normalized key → entry; on a collision (NFD/NFC or case twins) the first one seen is kept and recorded
pub(super) fn map_of<'a>(
    snap: &'a TableArtifact,
    kind: ObservedEntryKind,
    ci: bool,
    unread: &UnreadScope,
) -> (BTreeMap<String, &'a ObservedEntry>, Vec<String>) {
    let mut m: BTreeMap<String, &ObservedEntry> = BTreeMap::new();
    let mut dups = Vec::new();
    for e in snap.entries.iter().filter(|entry| entry.kind() == kind) {
        if unread.covers(e.path().as_str()) {
            continue;
        }
        let k = norm_key(e.path().as_str(), ci);
        match m.entry(k) {
            Entry::Occupied(_) => dups.push(e.path().as_str().to_owned()),
            Entry::Vacant(slot) => {
                slot.insert(e);
            }
        }
    }
    (m, dups)
}

// Evidence layer (read-only, for the UI; compare() is unaffected)
