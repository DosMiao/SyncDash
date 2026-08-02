//! The gates that must pass before anything is written.
//!
//! - `marker` proves a root is mounted with its data.
//! - `space` reserves the configured free-space margin.
//! - `ratio` blocks unexpectedly destructive plans.
//! - `scan` rejects incomplete evidence.
//! - `caps` reports unsupported backend requirements.
//! - `roots` probes reachability; `stats` computes plan totals.

pub mod caps;
pub mod marker;
pub mod ratio;
pub mod roots;
pub mod scan;
pub mod space;
pub mod stats;

use std::path::Path;

use crate::model::plan::{Op, PlanHeader};

use ratio::check_delete_ratio;
use roots::{check_root, check_root_vfs};
use scan::{check_materialized, check_scan_complete};
use space::{check_space, check_space_vfs};
use stats::stat_plan;

#[derive(Clone, Debug)]
pub struct Guards {
    /// Require a .syncdash-root marker on both roots (guards against an unmounted share)
    pub require_marker: bool,
    /// Minimum free ratio to keep (0.01 = 1%). <=0 disables
    pub min_free_pct: f64,
    /// Refuse to run when one side's deleted entries exceed this share of that side's total. <=0 or >=1 disables
    pub max_delete_ratio: f64,
    /// User allowed it through explicitly (--i-know); only lets the health-check gates pass, marker/space still block
    pub acknowledged: bool,
}

impl Default for Guards {
    fn default() -> Self {
        Guards {
            require_marker: false,
            min_free_pct: 0.01,
            max_delete_ratio: 0.5,
            acknowledged: false,
        }
    }
}

/// The verdict of one preflight. Non-empty `blockers` = refuse to run.
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

/// `run_all` over a backend pair: the same gates, with root and space checks going
/// through the VFS (an sftp root honestly reports "cannot determine" instead of a number).
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
        g,
        &mut v,
    );
    check_scan_complete(
        "target",
        head.target_walk_errors,
        &head.target_walk_err_samples,
        g,
        &mut v,
    );
    check_materialized(
        "source",
        head.source_icloud_stubs,
        &head.source_icloud_stub_samples,
        g,
        &mut v,
    );
    check_materialized(
        "target",
        head.target_icloud_stubs,
        &head.target_icloud_stub_samples,
        g,
        &mut v,
    );
    let st = stat_plan(ops);
    check_space_vfs(
        "target",
        target,
        st.target.write_bytes,
        g.min_free_pct,
        &mut v,
    );
    check_space_vfs(
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

/// The plan header carries every number the gates judge — entry counts, exclusions, walk errors —
/// so it is passed whole rather than unpacked at the call site. Two `u64` parameters in a row is
/// exactly the shape that lets source and target be handed over swapped.
pub fn run_all(
    ops: &[Op],
    source_root: &Path,
    target_root: &Path,
    head: &PlanHeader,
    g: &Guards,
) -> Verdict {
    let mut v = Verdict {
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    check_root("source", source_root, g.require_marker, &mut v);
    check_root("target", target_root, g.require_marker, &mut v);
    if !v.ok() {
        return v; // with a root unavailable, the later checks are meaningless
    }
    check_scan_complete(
        "source",
        head.source_walk_errors,
        &head.source_walk_err_samples,
        g,
        &mut v,
    );
    check_scan_complete(
        "target",
        head.target_walk_errors,
        &head.target_walk_err_samples,
        g,
        &mut v,
    );
    check_materialized(
        "source",
        head.source_icloud_stubs,
        &head.source_icloud_stub_samples,
        g,
        &mut v,
    );
    check_materialized(
        "target",
        head.target_icloud_stubs,
        &head.target_icloud_stub_samples,
        g,
        &mut v,
    );
    let st = stat_plan(ops);
    check_space(
        "target",
        target_root,
        st.target.write_bytes,
        g.min_free_pct,
        &mut v,
    );
    check_space(
        "source",
        source_root,
        st.source.write_bytes,
        g.min_free_pct,
        &mut v,
    );
    check_delete_ratio("target", &st.target, head.target_entries, g, &mut v);
    check_delete_ratio("source", &st.source, head.source_entries, g, &mut v);
    v
}
