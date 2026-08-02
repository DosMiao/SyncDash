//! The deletion-ratio gate. A plan that deletes most of a root is far more often a wrong filter,
//! a swapped source and target, or an unmounted share than an intended mass delete.

use super::stats::SideStats;
use super::Guards;
use super::Verdict;

/// Plan health check: deletion share. `entries` is that side's snapshot entry count (0 = nothing to judge against, skip).
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
    let msg = format!(
        "{label}: plan deletes {removals} of {entries} entries ({:.0}%) — over the {:.0}% guard. \
         A wrong filter, an unmounted share, or swapped source/target all look exactly like this.",
        ratio * 100.0,
        g.max_delete_ratio * 100.0
    );
    if g.acknowledged {
        v.warnings.push(format!("{msg} (allowed by --i-know)"));
    } else {
        v.blockers.push(format!(
            "{msg} Re-run with --i-know if this is really intended."
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::super::Guards;
    use super::*;

    #[test]
    fn delete_ratio_blocks_and_can_be_acknowledged() {
        let side = SideStats {
            deletes: 60,
            ..Default::default()
        };
        let g = Guards::default();
        let mut v = Verdict {
            blockers: vec![],
            warnings: vec![],
        };
        check_delete_ratio("target", &side, 100, &g, &mut v);
        assert_eq!(v.blockers.len(), 1, "60% deletion must be blocked");

        let g2 = Guards {
            acknowledged: true,
            ..Guards::default()
        };
        let mut v2 = Verdict {
            blockers: vec![],
            warnings: vec![],
        };
        check_delete_ratio("target", &side, 100, &g2, &mut v2);
        assert!(v2.ok(), "--i-know must let it through");
        assert_eq!(v2.warnings.len(), 1, "but it must still be reported");
    }

    #[test]
    fn small_deletions_pass() {
        let side = SideStats {
            deletes: 3,
            ..Default::default()
        };
        let mut v = Verdict {
            blockers: vec![],
            warnings: vec![],
        };
        check_delete_ratio("target", &side, 1000, &Guards::default(), &mut v);
        assert!(v.ok());
    }
}
