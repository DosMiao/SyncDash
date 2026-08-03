//! The deletion-share observation. A plan that deletes most of a root is far more often a wrong
//! filter, a swapped source and target, or an unmounted share than an intended mass delete — so it
//! is stated prominently and the review panel colors it. It does not withhold the run: the operator
//! reading the panel is the one who knows which of those it is.

use super::stats::SideStats;
use super::Guards;
use super::Verdict;

/// Deletion share. `entries` is that side's snapshot entry count (0 = nothing to judge against).
pub fn check_delete_ratio(
    label: &str,
    side: &SideStats,
    entries: u64,
    g: &Guards,
    v: &mut Verdict,
) {
    let removals = side.deletes;
    if removals == 0 || entries == 0 {
        return;
    }
    if !(g.max_delete_ratio > 0.0 && g.max_delete_ratio < 1.0) {
        return;
    }
    let ratio = removals as f64 / entries as f64;
    if ratio < g.max_delete_ratio {
        return;
    }
    v.warnings.push(format!(
        "{label}: plan deletes {removals} of {entries} entries ({:.0}%) — over the {:.0}% mark. \
         A wrong filter, an unmounted share, or swapped source/target all look exactly like this.",
        ratio * 100.0,
        g.max_delete_ratio * 100.0
    ));
}

#[cfg(test)]
mod tests {
    use super::super::Guards;
    use super::*;

    /// The share is evidence for the operator, not a decision the engine makes for them.
    #[test]
    fn a_mass_deletion_is_reported_without_withholding_the_run() {
        let side = SideStats {
            deletes: 60,
            ..Default::default()
        };
        let mut v = Verdict::default();
        check_delete_ratio("target", &side, 100, &Guards::default(), &mut v);
        assert!(v.ok(), "a deletion share must never refuse the run");
        assert_eq!(v.warnings.len(), 1, "and it must always be reported");
        assert!(v.warnings[0].contains("60 of 100"));
    }

    #[test]
    fn small_deletions_pass() {
        let side = SideStats {
            deletes: 3,
            ..Default::default()
        };
        let mut v = Verdict::default();
        check_delete_ratio("target", &side, 1000, &Guards::default(), &mut v);
        assert!(v.ok());
    }
}
