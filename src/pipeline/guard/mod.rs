//! The gates that must pass before anything is written.
//!
//! 1. **Mount-point marker** (`marker`) — semantics modelled on syncthing's `.stfolder`
//!    (CheckPath in `lib/config/folderconfiguration.go:236`). When an SMB share isn't mounted,
//!    target is usually an empty directory (it may even be auto-created locally), and mirror
//!    then plans "delete everything in target" or "re-send tens of GB". The marker file is the
//!    only reliable test: it travels with the **data**, so no mount means no marker.
//! 2. **Disk-space preflight** (`space`) — every op in the plan carries a size, so summing them
//!    up front tells us how much gets written. Modelled on syncthing's `CheckAvailableSpace` /
//!    `minDiskFree` (1% by default).
//! 3. **Plan health check** (`ratio`) — refuse to run when the deletion share is too high.
//!    syncthing has no equivalent (it syncs continuously; there is no "one big plan"), but our
//!    explicit model suits this gate well: it also catches a wrong filter, swapped source and
//!    target, and typo'd paths.
//!
//! `caps` is the fourth thing this module does, and the one the header used to leave out: the
//! capability report listing every gap between what a job asks for and what the two backends can
//! deliver, before any scanning starts. It shares no type with the three gates.
//!
//! `roots` probes reachability; `stats` reduces a plan to the totals the gates judge.

pub mod caps;
pub mod marker;
pub mod ratio;
pub mod roots;
pub mod space;
pub mod stats;

use std::path::Path;

use crate::model::plan::Op;

use ratio::check_delete_ratio;
use roots::{check_root, check_root_vfs};
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
        Guards { require_marker: false, min_free_pct: 0.01, max_delete_ratio: 0.5, acknowledged: false }
    }
}

/// The verdict of one preflight. Non-empty `blockers` = refuse to run.
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
    source_entries: u64,
    target_entries: u64,
    g: &Guards,
) -> Verdict {
    let mut v = Verdict { blockers: Vec::new(), warnings: Vec::new() };
    check_root_vfs("source", source, g.require_marker, &mut v);
    check_root_vfs("target", target, g.require_marker, &mut v);
    if !v.ok() {
        return v; // with a root unavailable, the later checks are meaningless
    }
    let st = stat_plan(ops);
    check_space_vfs("target", target, st.target.write_bytes, g.min_free_pct, &mut v);
    check_space_vfs("source", source, st.source.write_bytes, g.min_free_pct, &mut v);
    check_delete_ratio("target", &st.target, target_entries, g, &mut v);
    check_delete_ratio("source", &st.source, source_entries, g, &mut v);
    v
}

pub fn run_all(
    ops: &[Op],
    source_root: &Path,
    target_root: &Path,
    source_entries: u64,
    target_entries: u64,
    g: &Guards,
) -> Verdict {
    let mut v = Verdict { blockers: Vec::new(), warnings: Vec::new() };
    check_root("source", source_root, g.require_marker, &mut v);
    check_root("target", target_root, g.require_marker, &mut v);
    if !v.ok() {
        return v; // with a root unavailable, the later checks are meaningless
    }
    let st = stat_plan(ops);
    check_space("target", target_root, st.target.write_bytes, g.min_free_pct, &mut v);
    check_space("source", source_root, st.source.write_bytes, g.min_free_pct, &mut v);
    check_delete_ratio("target", &st.target, target_entries, g, &mut v);
    check_delete_ratio("source", &st.source, source_entries, g, &mut v);
    v
}
