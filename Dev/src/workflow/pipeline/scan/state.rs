//! How a scan interprets the acceleration tables it loaded, for both lanes.
//!
//! `store::hashcache` and `store::mtimefix` own the persisted formats and their reconciliation;
//! this module owns the three reading decisions a scan makes against them. All three answer from
//! values both lanes already hold — no store identity, no I/O — because the answers are not
//! allowed to depend on which lane asked: a corrected timestamp, a reusable digest, and the set of
//! prior paths outside the active filter domain mean the same thing over sftp as over a local disk.
//!
//! `local::state` binds these rules to a `LocalScanStateIdentity` and its `_local` store entry
//! points; the generic lane calls the same rules with its `_by_key` tables. Neither owns them.

use std::collections::HashSet;

use crate::foundation::path::RootRelativePath;
use crate::model::digest::Blake3Digest;
use crate::model::table::FileIdentityObservation;
use crate::store::hashcache::HashCache;
use crate::store::mtimefix::MtimeCorrections;

use super::digest::SAMPLE_MIN;
use super::ScanOptions;

/// The timestamp this scan reports for an object, and the record that it was corrected.
///
/// A correction only applies while the raw on-disk value still matches the one it was recorded
/// against; anything else means the object was written again by someone other than us, and the
/// correction now describes a file that is gone. Matching paths are collected because two later
/// decisions read that fact: the cache row for a corrected path cannot be trusted (see
/// `reusable_cached_digest`), and `store::mtimefix` reconciliation drops a correction whose object
/// this scan observed without matching it.
pub(super) fn resolve_mtime(
    corrections: &MtimeCorrections,
    matched: &mut HashSet<String>,
    relative: &str,
    raw_mtime_ms: i64,
) -> i64 {
    match corrections.get(relative) {
        Some(correction) if correction.ondisk_ms == raw_mtime_ms => {
            matched.insert(relative.to_owned());
            correction.intended_ms
        }
        _ => raw_mtime_ms,
    }
}

/// The cached digest this observation may reuse, if any.
///
/// The row has to match on size, on the reported mtime, and on the tier it was recorded at. The
/// condition that is easy to lose is the one before all three: a path whose mtime was corrected
/// reports the *intended* timestamp, so a replacement object that kept the same raw on-disk value
/// presents exactly the `(path, size, reported mtime)` triple the previous object cached. Such a
/// path is disqualified outright rather than validated.
///
/// `sampled` is the tier that will actually run rather than `options.sampled`, because the generic
/// lane may have been forced up to full reads by a backend without ranged reads. Reusing a sampled
/// digest under a full-read tier would hand it to `PendingFile::into_entry`, which would then label
/// three 256KB windows as a whole-file hash.
pub(super) fn reusable_cached_digest(
    cache: &HashCache,
    matched_mtime_fixes: &HashSet<String>,
    options: &ScanOptions,
    sampled: bool,
    relative: &str,
    size: u64,
    mtime_ms: i64,
) -> Option<Blake3Digest> {
    if !options.hash || !options.use_cache || matched_mtime_fixes.contains(relative) {
        return None;
    }
    let cached = cache.get(relative)?;
    let wants_sampled = sampled && size >= SAMPLE_MIN;
    let cached_is_sampled = matches!(
        cached.identity,
        FileIdentityObservation::SampledBlake3 { .. }
    );
    (cached.size == size && cached.mtime_ms == mtime_ms && cached_is_sampled == wants_sampled)
        .then(|| cached.identity.digest().cloned())
        .flatten()
}

/// Prior rows this scan deliberately did not look at, which reconciliation must therefore keep.
///
/// An error-free scan is entitled to prune acceleration state for paths it did not observe —
/// they are deletions. A path the filter excluded was never observed either, and pruning it would
/// silently discard state a later run with a wider filter still wants; naming those paths is what
/// this set is for. A partial scan returns nothing to name, because it saw too little to tell the
/// two apart and `store` already retains the whole prior table when coverage is not `Complete`.
pub(super) fn retain_absent(
    cache: &HashCache,
    corrections: &MtimeCorrections,
    coverage: crate::store::ScanCoverage,
    filter: &crate::pipeline::filter::PathFilter,
) -> HashSet<String> {
    if coverage != crate::store::ScanCoverage::Complete {
        return HashSet::new();
    }
    cache
        .keys()
        .map(RootRelativePath::as_str)
        .chain(corrections.keys().map(String::as_str))
        .filter(|path| !filter.pass_file(path))
        .map(str::to_owned)
        .collect()
}
