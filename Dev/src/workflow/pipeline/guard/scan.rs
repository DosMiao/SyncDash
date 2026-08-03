//! The gate that asks whether the scan this plan was built from actually saw the whole tree.
//!
//! Every other gate here judges the plan. This one judges the *evidence*, because a plan built on
//! a partial scan is not a wrong plan — it is a plausible one. Compare cannot distinguish "this
//! file is gone" from "this file was never read": both are an absent entry, and under mirror both
//! produce a delete. The scan lanes refuse to emit a table whose subtrees are missing, so anything
//! that survives to here is a race (an entry that vanished mid-walk) — rare, self-limiting, and
//! still worth refusing over, because the one case that matters is indistinguishable from the many
//! that do not.

use super::Verdict;

/// Refuse a plan whose scan skipped entries. `--i-know` downgrades it to a warning, matching the
/// deletion-ratio gate: the user is the only one who can say a skipped entry was not a file the
/// other side is about to lose.
pub fn check_scan_complete(label: &str, walk_errors: u64, samples: &[String], v: &mut Verdict) {
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
    v.warnings.push(format!(
        "{msg} Re-scan if those entries are not really gone."
    ));
}

/// Refuse a plan built over iCloud placeholders.
///
/// A `.<name>.icloud` stub is not a damaged file, it is a *different* file: a few hundred bytes of
/// plist standing where the real one was. The scan cannot see the original, so mirror reads it as
/// deleted and plans a copy of the stub plus a delete of the real copy on the other side. The
/// original survives in the trash store until it is pruned, and every counter still reports success.
///
/// Excluding the stub does not help and is worth saying out loud, because it is the obvious move:
/// the delete is driven by the absence of the real name, so hiding the stub only removes the hint.
pub fn check_materialized(label: &str, stubs: u64, samples: &[String], v: &mut Verdict) {
    if stubs == 0 {
        return;
    }
    let named = if samples.is_empty() {
        String::new()
    } else {
        format!(" For example: {}.", samples.join(" | "))
    };
    let msg = format!(
        "{label}: {stubs} file(s) are iCloud placeholders — their contents are not on this disk, \
         and their real names are absent from this side's table.{named} Synchronizing would copy \
         the placeholder over the real file on the other side and delete the original. Excluding \
         them does not help: the delete comes from the missing name, not from the placeholder."
    );
    v.warnings.push(format!(
        "{msg} Download them first (Finder, or `brctl download <path>`), or exclude the whole tree."
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_reported_and_say_why_excluding_them_will_not_help() {
        let mut v = Verdict {
            blockers: vec![],
            warnings: vec![],
        };
        let s = ["Docs/.Report.pdf.icloud".to_string()];
        check_materialized("source", 1, &s, &mut v);
        assert!(v.ok(), "placeholders are reported, not refused");
        assert_eq!(v.warnings.len(), 1);
        assert!(v.warnings[0].contains("Docs/.Report.pdf.icloud"));
        // The remedy has to be in the message: the obvious move (exclude it) is the wrong one.
        assert!(v.warnings[0].contains("Excluding them does not help"));
    }

    #[test]
    fn a_materialized_tree_says_nothing() {
        let mut v = Verdict {
            blockers: vec![],
            warnings: vec![],
        };
        check_materialized("source", 0, &[], &mut v);
        assert!(v.ok() && v.warnings.is_empty());
    }

    #[test]
    fn a_complete_scan_says_nothing() {
        let mut v = Verdict {
            blockers: vec![],
            warnings: vec![],
        };
        check_scan_complete("source", 0, &[], &mut v);
        assert!(v.ok());
        assert!(v.warnings.is_empty(), "a clean scan must not produce noise");
    }

    #[test]
    fn one_skipped_entry_blocks_and_names_it() {
        let mut v = Verdict {
            blockers: vec![],
            warnings: vec![],
        };
        let samples = ["/Users/x/Desktop: Operation not permitted (os error 1)".to_string()];
        check_scan_complete("source", 1, &samples, &mut v);
        assert!(v.ok(), "unread entries are reported, not refused");
        assert_eq!(
            v.warnings.len(),
            1,
            "a single skipped entry is enough to report"
        );
        // The warning has to carry the path: "1 entry skipped" sends the user hunting, the path
        // sends them to Privacy & Security.
        assert!(v.warnings[0].contains("/Users/x/Desktop"));
    }
}
