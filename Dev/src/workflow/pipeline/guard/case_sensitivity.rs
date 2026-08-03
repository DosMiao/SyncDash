//! The gate that asks whether a root can hold the case distinction the job declares.
//!
//! `case_sensitive` is a *declaration*: `CompareOptions.case_insensitive` decides how names are
//! matched, and it keeps deciding that here — a measurement never overrides it, because a user who
//! sets `case_sensitive = true` is asking for the collision refusals in
//! `compare::planning::names`, and silently folding their names back together would remove exactly
//! the protection they asked for.
//!
//! `VfsCaps.case_sensitivity` is *evidence*, and it is only ever consulted when it settles the
//! question. `CaseSense::Unknown` means "not probed" (`base::fs::vfs`), which is the answer for
//! APFS, HFS+, and every protocol backend, so it says nothing at all. A measured `Insensitive`
//! against a declared-sensitive job is the one case where the two disagree on a fact, and that is
//! reported rather than guessed past.
//!
//! Reported, not refused: the volume table is a filesystem-name heuristic, the user may know their
//! mount better than it does, and the writes that would actually lose bytes are already refused as
//! conflicts at plan time. An unattended AutoScan Apply, which has no operator to weigh it, refuses
//! on warnings — which is the asymmetry this belongs on.

use super::Verdict;
use crate::fs::vfs::CaseSense;

pub(super) fn check_case_declaration(
    label: &str,
    root: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    declared_case_sensitive: bool,
    out: &mut Verdict,
) {
    if !declared_case_sensitive {
        return;
    }
    if root.caps().case_sensitivity != CaseSense::Insensitive {
        return;
    }
    out.warnings.push(format!(
        "{label}: this job declares case_sensitive = true, but {} measures as a case-insensitive \
         filesystem — names there that differ only in case are one entry, so a pair this job \
         compares as two separate files cannot both exist on this root. Planning refuses every \
         write that would fold onto another rather than picking a winner, so those pairs arrive as \
         conflicts. Set case_sensitive = false to match this volume, or rename one of each pair.",
        root.display()
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::memory::MemVfs;
    use crate::fs::vfs::Vfs;

    fn root(case: CaseSense) -> std::sync::Arc<dyn Vfs> {
        std::sync::Arc::new(MemVfs::new("mem").without(|caps| caps.case_sensitivity = case))
    }

    #[test]
    fn a_declaration_the_measured_volume_contradicts_is_reported() {
        let mut verdict = Verdict::default();
        check_case_declaration("target", &root(CaseSense::Insensitive), true, &mut verdict);
        assert!(
            verdict.ok(),
            "a declaration mismatch must never refuse the run"
        );
        assert_eq!(verdict.warnings.len(), 1, "{verdict:?}");
        assert!(
            verdict.warnings[0].starts_with("target:"),
            "the message has to name the side it measured: {}",
            verdict.warnings[0]
        );
        assert!(
            verdict.warnings[0].contains("case_sensitive"),
            "the message has to name the setting the user would change: {}",
            verdict.warnings[0]
        );
    }

    #[test]
    fn an_unprobed_volume_says_nothing() {
        let mut verdict = Verdict::default();
        check_case_declaration("target", &root(CaseSense::Unknown), true, &mut verdict);
        assert_eq!(
            verdict,
            Verdict::default(),
            "Unknown is the honest not-probed answer, not evidence of a contradiction"
        );
    }

    #[test]
    fn a_case_insensitive_job_says_nothing() {
        let mut verdict = Verdict::default();
        check_case_declaration("target", &root(CaseSense::Insensitive), false, &mut verdict);
        assert_eq!(
            verdict,
            Verdict::default(),
            "the default job agrees with the volume, so there is nothing to report"
        );
    }

    #[test]
    fn a_measured_case_sensitive_volume_says_nothing() {
        let mut verdict = Verdict::default();
        check_case_declaration("source", &root(CaseSense::Sensitive), true, &mut verdict);
        assert_eq!(
            verdict,
            Verdict::default(),
            "the measurement confirms the declaration"
        );
    }
}
