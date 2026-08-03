//! Projection from a validated job configuration into workflow-layer option types.

use std::path::PathBuf;

use crate::job::model::Job;
use crate::job::rigor::RigorResolved;

impl Job {
    /// Resolve a rigor preset plus any explicit detail overrides.
    pub fn rigor_resolved(&self) -> RigorResolved {
        RigorResolved::from_preset(&self.rigor)
            .with_evidence(self.evidence.as_deref())
            .with_cache(self.use_cache)
            .with_escalate(self.escalate)
            .with_verify_writes(self.verify_writes)
    }

    /// The read-side capability query, with its timestamp window already widened to the coarser
    /// backend precision.
    pub fn read_caps_query(
        &self,
        window_ms: i64,
        src_local: bool,
        tgt_local: bool,
    ) -> crate::pipeline::guard::caps::ReadCapsQuery {
        let rr = self.rigor_resolved();
        crate::pipeline::guard::caps::ReadCapsQuery {
            hash: rr.hash,
            sampled: rr.sampled,
            escalate: rr.escalate,
            symlinks_direct: self.symlinks == "direct",
            min_free_pct: self.min_free_pct,
            window_ms,
            src_local,
            tgt_local,
        }
    }

    /// The write-side capability query.
    pub fn write_caps_query(
        &self,
        src_local: bool,
        tgt_local: bool,
    ) -> crate::pipeline::guard::caps::WriteCapsQuery {
        crate::pipeline::guard::caps::WriteCapsQuery {
            fsync: self.fsync,
            verify: self.rigor_resolved().verify_writes,
            versioning: self.versioning,
            delta: self.delta,
            src_local,
            tgt_local,
        }
    }

    pub fn guards(&self) -> crate::pipeline::guard::Guards {
        crate::pipeline::guard::Guards {
            require_marker: self.require_marker,
            min_free_pct: self.min_free_pct,
            max_delete_ratio: self.max_delete_ratio,
        }
    }

    pub fn compare_opts(&self) -> crate::pipeline::compare::CompareOptions {
        crate::pipeline::compare::CompareOptions {
            case_insensitive: !self.case_sensitive,
            conflict: match self.on_conflict.as_str() {
                "copy" => crate::pipeline::compare::ConflictPolicy::Copy,
                "newer" => crate::pipeline::compare::ConflictPolicy::Newer,
                _ => crate::pipeline::compare::ConflictPolicy::Report,
            },
            sync_mode: self.sync_mode,
            max_conflicts: self.max_conflicts,
            // Root resolution widens this to the coarser backend precision.
            mtime_window_ms: crate::pipeline::compare::MTIME_SLACK_MS,
        }
    }

    pub fn apply_opts(
        &self,
        trash: Option<PathBuf>,
        verbose: bool,
    ) -> crate::pipeline::apply::ApplyOptions {
        crate::pipeline::apply::ApplyOptions {
            dry_run: false,
            trash,
            verbose,
            verify: self.rigor_resolved().verify_writes,
            versioning: self.versioning,
            fsync: self.fsync,
            filter: Some(crate::pipeline::filter::PathFilter::build_full(
                &self.include,
                &self.exclude,
                &self.deletable,
            )),
            delta: self.delta,
            parallel: self.parallel.unwrap_or(4).clamp(1, 16),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(rigor: &str) -> Job {
        Job {
            rigor: rigor.into(),
            ..Default::default()
        }
    }

    #[test]
    fn job_resolution_applies_detail_overrides() {
        let mut j = job("fast");
        j.evidence = Some("full".into());
        j.use_cache = Some(false);
        j.verify_writes = Some(true);
        let r = j.rigor_resolved();
        assert!(r.hash && !r.sampled && !r.use_cache && r.verify_writes);

        let mut c = job("custom");
        c.evidence = Some("none".into());
        let rc = c.rigor_resolved();
        assert!(!rc.hash);
        assert!(rc.verify_writes, "custom base inherits standard verify");
    }
}
