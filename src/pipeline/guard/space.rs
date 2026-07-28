//! The free-space gate. The probe itself is `foundation::disk`; the policy is here — how much
//! headroom a plan needs, and what to say when it is not there.

use std::path::Path;

use super::Verdict;
use crate::foundation::fmt::human_bytes;

/// Space check: the writing side needs write_bytes, and must still have min_free_pct left afterwards.
pub fn check_space(label: &str, root: &Path, need: u64, min_free_pct: f64, v: &mut Verdict) {
    if need == 0 {
        return;
    }
    let Some((avail, total)) = crate::foundation::disk::disk_space(root) else {
        v.warnings.push(format!("{label}: cannot determine free space on {}", root.display()));
        return;
    };
    // 10% margin: the target may have cluster alignment / sparseness / metadata overhead, and the sizes in the plan are the source side's
    let need_padded = need.saturating_add(need / 10);
    let reserve = if min_free_pct > 0.0 { (total as f64 * min_free_pct) as u64 } else { 0 };
    if avail < need_padded.saturating_add(reserve) {
        v.blockers.push(format!(
            "{label}: insufficient space on {} — need {} (+10% margin) and want {} free afterwards, but only {} available",
            root.display(),
            human_bytes(need),
            human_bytes(reserve),
            human_bytes(avail),
        ));
    }
}
/// Run every gate in one pass. `source_entries` / `target_entries` come from the two snapshots.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_space_vfs(
    label: &str,
    v: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    need: u64,
    min_free_pct: f64,
    out: &mut Verdict,
) {
    if need == 0 {
        return;
    }
    let Some((avail, total)) = v.free_space().ok().flatten() else {
        out.warnings.push(format!("{label}: cannot determine free space on {}", v.display()));
        return;
    };
    let need_padded = need.saturating_add(need / 10);
    let reserve = if min_free_pct > 0.0 { (total as f64 * min_free_pct) as u64 } else { 0 };
    if avail < need_padded.saturating_add(reserve) {
        out.blockers.push(format!(
            "{label}: not enough free space on {}: writing needs ~{} (plus {} reserve), only {} available",
            v.display(),
            human_bytes(need_padded),
            human_bytes(reserve),
            human_bytes(avail)
        ));
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn disk_space_reports_something_for_temp_dir() {
        // Don't assert specific numbers, only that this machine can report them — failing to would degrade to a warning, not a block
        let got = crate::foundation::disk::disk_space(&std::env::temp_dir());
        assert!(got.is_some(), "free space query should work on the temp volume");
        let (avail, total) = got.unwrap();
        assert!(total > 0 && avail <= total);
    }
}
