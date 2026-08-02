//! Loading, interpreting, and publishing per-root scanner acceleration state.

use std::collections::{HashMap, HashSet};

use crate::model::table::{FileIdentityObservation, ObservedEntry};

use super::super::digest::SAMPLE_MIN;
use super::super::{ScanMetrics, ScanOptions};
use super::model::PendingFile;

pub(super) struct LocalScanState {
    identity: crate::store::localid::LocalScanStateIdentity,
    cache: crate::store::hashcache::HashCache,
    mtime_fixes: HashMap<String, (i64, i64)>,
    matched_mtime_fixes: HashSet<String>,
}

impl LocalScanState {
    pub(super) fn load(
        root: &crate::fs::local_root::LocalRoot,
        options: &ScanOptions,
        metrics: &mut ScanMetrics,
    ) -> Self {
        let identity = crate::store::localid::LocalScanStateIdentity::for_root(root.display_path());
        let measured = std::time::Instant::now();
        // A no-cache rigor still loads the previous table so rows outside the current filter domain
        // survive reconciliation; no observed file is allowed to reuse those hashes below.
        let cache = if options.hash {
            crate::store::hashcache::load_local(&identity)
        } else {
            HashMap::new()
        };
        metrics.cache_load_ms = measured.elapsed().as_millis() as u64;

        let measured = std::time::Instant::now();
        let mtime_fixes = crate::store::mtimefix::load_local(&identity);
        metrics.mtime_load_ms = measured.elapsed().as_millis() as u64;

        Self {
            identity,
            cache,
            mtime_fixes,
            matched_mtime_fixes: HashSet::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_file(
        &mut self,
        relative: crate::foundation::path::RootRelativePath,
        size: u64,
        raw_mtime_ms: i64,
        observed_file_id: Option<String>,
        mode: Option<u32>,
        options: &ScanOptions,
    ) -> PendingFile {
        let relative_text = relative.as_str();
        let mtime_ms = match self.mtime_fixes.get(relative_text) {
            Some((ondisk, intended)) if *ondisk == raw_mtime_ms => {
                self.matched_mtime_fixes.insert(relative_text.to_owned());
                *intended
            }
            _ => raw_mtime_ms,
        };
        let hash = if options.hash
            && options.use_cache
            && !self.matched_mtime_fixes.contains(relative_text)
        {
            self.cache.get(relative_text).and_then(|cached| {
                let wants_sampled = options.sampled && size >= SAMPLE_MIN;
                let cached_is_sampled = matches!(
                    cached.identity,
                    FileIdentityObservation::SampledBlake3 { .. }
                );
                (cached.size == size
                    && cached.mtime_ms == mtime_ms
                    && cached_is_sampled == wants_sampled)
                    .then(|| cached.identity.digest().cloned())
                    .flatten()
            })
        } else {
            None
        };
        PendingFile {
            relative,
            size,
            raw_mtime_ms,
            mtime_ms,
            hash,
            hash_failed: false,
            file_id: self
                .identity
                .file_ids_stable()
                .then_some(observed_file_id.clone())
                .flatten(),
            observed_file_id,
            mode,
        }
    }

    pub(super) fn retain_absent(
        &self,
        coverage: crate::store::ScanCoverage,
        options: &ScanOptions,
    ) -> HashSet<String> {
        if coverage != crate::store::ScanCoverage::Complete {
            return HashSet::new();
        }
        self.cache
            .keys()
            .map(crate::foundation::path::RootRelativePath::as_str)
            .chain(self.mtime_fixes.keys().map(String::as_str))
            .filter(|path| !options.filter.pass_file(path))
            .map(str::to_owned)
            .collect()
    }

    pub(super) fn publish(
        &self,
        entries: &[ObservedEntry],
        coverage: crate::store::ScanCoverage,
        retain_absent: &HashSet<String>,
        options: &ScanOptions,
        metrics: &mut ScanMetrics,
    ) {
        if options.hash
            && crate::store::hashcache::save_local(&self.identity, entries, coverage, retain_absent)
                == crate::store::StateWriteStatus::Failed
        {
            metrics.state_failures += 1;
        }
        if crate::store::mtimefix::reconcile_local(
            &self.identity,
            &self.mtime_fixes,
            entries,
            coverage,
            &self.matched_mtime_fixes,
            retain_absent,
        ) == crate::store::StateWriteStatus::Failed
        {
            metrics.state_failures += 1;
        }
    }
}
