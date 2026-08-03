//! The gates that must pass before anything is written.
//!
//! - `marker` proves a root is mounted with its data.
//! - `space` reserves the configured free-space margin.
//! - `ratio` reports unexpectedly destructive plans.
//! - `scan` reports incomplete evidence.
//! - `caps` reports unsupported backend requirements.
//! - `roots` probes reachability; `stats` computes plan totals.

pub mod caps;
pub mod marker;
pub mod ratio;
pub mod roots;
pub mod scan;
pub mod space;
pub mod stats;

use crate::model::plan::{Op, PlanHeader};

use ratio::check_delete_ratio;
use roots::check_root_vfs;
use scan::{check_materialized, check_scan_complete};
use space::check_space;
use stats::stat_plan;

#[derive(Clone, Debug)]
pub struct Guards {
    /// Require a .syncdash-root marker on both roots (guards against an unmounted share)
    pub require_marker: bool,
    /// Minimum free ratio to keep (0.01 = 1%). <=0 disables
    pub min_free_pct: f64,
    /// Report a side's deletions prominently once they exceed this share of that side's total, and
    /// color the matching review row. <=0 or >=1 disables. Never withholds the run.
    pub max_delete_ratio: f64,
}

impl Default for Guards {
    fn default() -> Self {
        Guards {
            require_marker: false,
            min_free_pct: 0.01,
            max_delete_ratio: 0.5,
        }
    }
}

/// The verdict of one preflight. `blockers` carry the preconditions a run cannot proceed without —
/// a root that is not there, or a disk that cannot hold the writes. Everything the operator should
/// weigh rather than have decided for them arrives as a `warning` instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Verdict {
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

impl Verdict {
    pub fn ok(&self) -> bool {
        self.blockers.is_empty()
    }
    /// Print the verdict to stderr; returns whether it is allowed through
    pub fn report(&self, tag: &str) -> bool {
        for w in &self.warnings {
            crate::log_warn!("preflight", "[{tag}] warning: {w}");
        }
        for b in &self.blockers {
            crate::log_error!("preflight", "[{tag}] REFUSED: {b}");
        }
        self.ok()
    }
}

/// Every gate in one pass over a backend pair. The plan header carries every number the gates
/// judge — entry counts, exclusions, walk errors — so it is passed whole rather than unpacked at
/// the call site.
///
/// Root and space checks go through the `Vfs`, so an sftp root honestly reports "cannot determine"
/// instead of a number it cannot know.
pub fn run_all_vfs(
    ops: &[Op],
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    head: &PlanHeader,
    g: &Guards,
) -> Verdict {
    let mut v = Verdict {
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    check_root_vfs("source", source, g.require_marker, &mut v);
    check_root_vfs("target", target, g.require_marker, &mut v);
    if !v.ok() {
        return v; // with a root unavailable, the later checks are meaningless
    }
    check_scan_complete(
        "source",
        head.source_walk_errors,
        &head.source_walk_err_samples,
        &mut v,
    );
    check_scan_complete(
        "target",
        head.target_walk_errors,
        &head.target_walk_err_samples,
        &mut v,
    );
    check_materialized(
        "source",
        head.source_icloud_stubs,
        &head.source_icloud_stub_samples,
        &mut v,
    );
    check_materialized(
        "target",
        head.target_icloud_stubs,
        &head.target_icloud_stub_samples,
        &mut v,
    );
    let st = stat_plan(ops);
    check_space(
        "target",
        target,
        st.target.write_bytes,
        g.min_free_pct,
        &mut v,
    );
    check_space(
        "source",
        source,
        st.source.write_bytes,
        g.min_free_pct,
        &mut v,
    );
    check_delete_ratio("target", &st.target, head.target_entries, g, &mut v);
    check_delete_ratio("source", &st.source, head.source_entries, g, &mut v);
    v
}
