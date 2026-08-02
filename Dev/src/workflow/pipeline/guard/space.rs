//! The free-space gate. The probe itself is `foundation::disk`; the policy is here — how much
//! headroom a plan needs, and what to say when it is not there.

use super::Verdict;
use crate::foundation::fmt::human_bytes;

/// The writing side needs `need` bytes, and must still have `min_free_pct` of the volume free
/// afterwards. A backend that cannot report free space degrades to a warning, never a block —
/// refusing a plan because a protocol root will not answer would strand every sftp job.
pub(super) fn check_space(
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
        out.warnings.push(format!(
            "{label}: cannot determine free space on {}",
            v.display()
        ));
        return;
    };
    let need_padded = need.saturating_add(need / 10);
    let reserve = if min_free_pct > 0.0 {
        (total as f64 * min_free_pct) as u64
    } else {
        0
    };
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
        assert!(
            got.is_some(),
            "free space query should work on the temp volume"
        );
        let (avail, total) = got.unwrap();
        assert!(total > 0 && avail <= total);
    }
}
