//! The gate that asks whether the scan this plan was built from actually saw the whole tree.
//!
//! Every other gate here judges the plan. This one judges the *evidence*, because a plan built on
//! a partial scan is not a wrong plan — it is a plausible one. Compare cannot distinguish "this
//! file is gone" from "this file was never read": both are an absent entry, and under mirror both
//! produce a delete. The scan lanes refuse to emit a table whose subtrees are missing, so anything
//! that survives to here is a race (an entry that vanished mid-walk) — rare, self-limiting, and
//! still worth refusing over, because the one case that matters is indistinguishable from the many
//! that do not.

use super::{Guards, Verdict};

/// Refuse a plan whose scan skipped entries. `--i-know` downgrades it to a warning, matching the
/// deletion-ratio gate: the user is the only one who can say a skipped entry was not a file the
/// other side is about to lose.
pub fn check_scan_complete(
    label: &str,
    walk_errors: u64,
    samples: &[String],
    g: &Guards,
    v: &mut Verdict,
) {
    if walk_errors == 0 {
        return;
    }
    let named = if samples.is_empty() {
        String::new()
    } else {
        format!(" Could not read: {}.", samples.join(" | "))
    };
    let msg = format!(
        "{label}: the scan skipped {walk_errors} entr(ies) it could not read, so they are absent \
         from this side's table — which compare reads as deleted, not as unseen.{named}"
    );
    if g.acknowledged {
        v.warnings.push(format!("{msg} (allowed by --i-know)"));
    } else {
        v.blockers.push(format!(
            "{msg} Re-scan, or re-run with --i-know if those entries really are gone."
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_complete_scan_says_nothing() {
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        check_scan_complete("source", 0, &[], &Guards::default(), &mut v);
        assert!(v.ok());
        assert!(v.warnings.is_empty(), "a clean scan must not produce noise");
    }

    #[test]
    fn one_skipped_entry_blocks_and_names_it() {
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        let samples = ["/Users/x/Desktop: Operation not permitted (os error 1)".to_string()];
        check_scan_complete("source", 1, &samples, &Guards::default(), &mut v);
        assert_eq!(v.blockers.len(), 1, "a single skipped entry is enough to refuse");
        // The blocker has to carry the path: "1 entry skipped" sends the user hunting, the path
        // sends them to Privacy & Security.
        assert!(v.blockers[0].contains("/Users/x/Desktop"));
    }

    #[test]
    fn i_know_downgrades_it_but_never_hides_it() {
        let g = Guards { acknowledged: true, ..Guards::default() };
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        check_scan_complete("target", 4, &[], &g, &mut v);
        assert!(v.ok());
        assert_eq!(v.warnings.len(), 1);
    }
}
