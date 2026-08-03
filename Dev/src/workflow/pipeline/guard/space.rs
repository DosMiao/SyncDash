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
    use super::*;
    use crate::fs::vfs::memory::MemVfs;
    use crate::fs::vfs::Vfs;

    fn volume() -> std::sync::Arc<dyn Vfs> {
        std::sync::Arc::new(MemVfs::new("mem"))
    }

    /// What the fixture volume says about itself, so these tests judge the policy rather than a
    /// restatement of `MemVfs`.
    fn available(volume: &std::sync::Arc<dyn Vfs>) -> u64 {
        volume
            .free_space()
            .expect("MemVfs answers the free-space probe")
            .expect("MemVfs reports free space")
            .0
    }

    /// The gate has to be able to say no. Everything else in this module — the padding, the
    /// reserve, the degraded-backend warning — only matters if a plan that cannot fit is refused.
    #[test]
    fn a_plan_larger_than_the_volume_is_blocked() {
        let volume = volume();
        let mut verdict = Verdict::default();
        check_space(
            "target",
            &volume,
            available(&volume) * 2,
            0.01,
            &mut verdict,
        );
        assert_eq!(verdict.blockers.len(), 1, "{verdict:?}");
        assert!(
            verdict.blockers[0].contains("not enough free space"),
            "{}",
            verdict.blockers[0]
        );
        assert!(verdict.warnings.is_empty(), "{verdict:?}");
    }

    /// The reserve is the whole point of `min_free_pct`: a plan that fits into the last of the
    /// volume still has to leave the configured margin behind.
    #[test]
    fn the_reserve_is_what_refuses_a_plan_that_would_otherwise_just_fit() {
        let volume = volume();
        // Padded by the 10% check_space adds, this lands just inside the volume and just outside
        // the 1% reserve.
        let need = available(&volume) / 11 * 10 - 1_000_000_000;

        let mut without_reserve = Verdict::default();
        check_space("target", &volume, need, 0.0, &mut without_reserve);
        assert!(without_reserve.ok(), "{without_reserve:?}");

        let mut with_reserve = Verdict::default();
        check_space("target", &volume, need, 0.01, &mut with_reserve);
        assert_eq!(with_reserve.blockers.len(), 1, "{with_reserve:?}");
    }

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
