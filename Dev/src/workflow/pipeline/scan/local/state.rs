//! This root's binding to the scanner acceleration tables: which identity they belong to, when
//! they may be reused at all, and publishing what the scan observed back into them.
//!
//! The interpretation rules themselves are `scan::state`, shared with the generic lane. What is
//! local here is the `LocalScanStateIdentity` — the `_local` store entry points, and
//! `file_ids_stable`, which is a fact this filesystem reports about itself rather than a scanning
//! decision, and so does not belong in a rule both lanes run.

use std::collections::{HashMap, HashSet};

use crate::model::table::ObservedEntry;

use super::super::model::PendingFile;
use super::super::{state as rules, ScanMetrics, ScanOptions};

pub(super) struct LocalScanState {
    identity: crate::store::localid::LocalScanStateIdentity,
    cache: crate::store::hashcache::HashCache,
    mtime_fixes: crate::store::mtimefix::MtimeCorrections,
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
        let mtime_ms = rules::resolve_mtime(
            &self.mtime_fixes,
            &mut self.matched_mtime_fixes,
            relative_text,
            raw_mtime_ms,
        );
        // This lane always runs the tier it was asked for: a retained local root has every
        // primitive, so nothing can force it up from sampled to full reads mid-scan.
        let hash = rules::reusable_cached_digest(
            &self.cache,
            &self.matched_mtime_fixes,
            options,
            options.sampled,
            relative_text,
            size,
            mtime_ms,
        );
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
        rules::retain_absent(&self.cache, &self.mtime_fixes, coverage, &options.filter)
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
