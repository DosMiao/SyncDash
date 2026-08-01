//! The gates that refuse, and the precise line between them.
//!
//! A delete ratio is a judgment call about *this plan*, so a person may overrule it with
//! `--i-know`. A missing mount marker is a fact about *the environment* — the share is very likely
//! not mounted — and no conviction makes an unmounted share safe to mirror onto, so it is checked
//! earlier, at compare, and refuses before a plan even exists. Keeping those two apart is the whole
//! reason they are separate flags rather than one "force".

use std::sync::Arc;

use super::*;
use crate::fs::vfs::memory::MemVfs;
use crate::fs::vfs::Vfs;

/// Ten files, all at the root. Flat on purpose: the guard divides deletions by the side's whole
/// snapshot entry count, and directories inflate that denominator while only files produce `Delete`
/// ops. With a flat tree, "8 of 10" is exactly what the guard sees, so the case tests the gate
/// rather than arithmetic about the fixture.
const FLAT: &[Seed] = &[
    Seed {
        path: "f00.txt",
        seed: 100,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "f01.txt",
        seed: 101,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "f02.txt",
        seed: 102,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "f03.txt",
        seed: 103,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "f04.txt",
        seed: 104,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "f05.txt",
        seed: 105,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "f06.txt",
        seed: 106,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "f07.txt",
        seed: 107,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "f08.txt",
        seed: 108,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
    Seed {
        path: "f09.txt",
        seed: 109,
        size: 512,
        mtime_ms: 1_767_225_600_000,
    },
];

/// Both roots hold `FLAT`; the source keeps only two files. Eight of the target's ten entries are
/// now missing — which is what a wrong filter, a swapped pair of roots, or an unmounted share all
/// look like from inside the plan.
fn mass_delete_pair() -> (Arc<dyn Vfs>, Arc<dyn Vfs>) {
    let sv: Arc<dyn Vfs> = Arc::new(MemVfs::new("guard-src"));
    let tv: Arc<dyn Vfs> = Arc::new(MemVfs::new("guard-tgt"));
    corpus::seed_into(&sv, FLAT);
    corpus::seed_into(&tv, FLAT);
    for s in &FLAT[2..] {
        sv.remove_file(s.path).expect("strip the source");
    }
    (sv, tv)
}

fn guarded_job(max_delete_ratio: f64, require_marker: bool) -> Job {
    Job {
        max_delete_ratio,
        require_marker,
        ..bare_job()
    }
}

/// The guard that exists because a wrong filter and a real mass deletion are indistinguishable from
/// inside the plan. It refuses, and nothing is touched.
#[test]
fn the_delete_ratio_guard_blocks_a_mass_delete() {
    let (sv, tv) = mass_delete_pair();
    let before = tree::shape_of(&tv);
    let (plan, ap, said) = try_cycle(&guarded_job(0.5, false), &sv, &tv, false);

    assert_eq!(
        plan.ops.len(),
        8,
        "the plan really does propose all eight deletions"
    );
    assert_eq!(ap.errors, 1, "the run must refuse\n{said}");
    assert_eq!(ap.done, 0, "and must not have deleted anything first");
    assert!(
        said.contains("over the 50% guard"),
        "the refusal has to say what tripped and why:\n{said}"
    );
    assert_eq!(
        tree::shape_of(&tv),
        before,
        "the target is untouched after a refusal"
    );
}

/// `--i-know` is the user overruling a judgment call. It turns that one blocker into a warning and
/// the deletions go through.
#[test]
fn the_delete_ratio_guard_yields_to_i_know() {
    let (sv, tv) = mass_delete_pair();
    let (_, ap, said) = try_cycle(&guarded_job(0.5, false), &sv, &tv, true);
    assert_eq!(ap.errors, 0, "acknowledged, it should proceed\n{said}");
    assert!(
        ap.done >= 8,
        "every proposed deletion should have run, got {}",
        ap.done
    );
    let tol = tree::Tolerance::between(&sv, &tv);
    tree::assert_same(
        &tree::shape_of(&sv),
        &tree::shape_of(&tv),
        &tol,
        "after --i-know",
    );
}

/// The marker gate fires at *compare*, before a plan exists — so there is nothing to acknowledge,
/// and `--i-know` cannot reach it however emphatically it is passed.
#[test]
fn i_know_does_not_reach_the_mount_marker() {
    let (sv, tv) = mass_delete_pair();
    let (_, ctx) = watched();
    // `expect_err` would need `CompareOutcome: Debug`; a shipped type does not grow a derive to
    // suit a test.
    let Err(err) =
        crate::run::local::compare_resolved(&guarded_job(0.0, true), &sv, &tv, &ctx, true)
    else {
        panic!("a missing marker must refuse even with --i-know in hand");
    };
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    let msg = err.to_string();
    assert!(
        msg.contains(".syncdash-root") && msg.contains("syncdash mark"),
        "the refusal has to name the marker and how to create it: {msg}"
    );
}

/// The deliberate non-block: an empty, unmarked target is suspicious but legal, so it warns and
/// proceeds. Pinned because the decision *not* to block here is a real one — turning it into an
/// error is what `require_marker` is for.
#[test]
fn an_empty_unmarked_target_warns_but_proceeds() {
    let sv: Arc<dyn Vfs> = Arc::new(MemVfs::new("empty-src"));
    let tv: Arc<dyn Vfs> = Arc::new(MemVfs::new("empty-tgt"));
    corpus::seed_into(&sv, corpus::BASE);
    let (_, ap, said) = try_cycle(&guarded_job(0.0, false), &sv, &tv, false);
    assert_eq!(
        ap.errors, 0,
        "an empty target is filled, not refused\n{said}"
    );
    assert!(
        said.contains("empty and unmarked"),
        "but it must say so, because an unmounted share looks exactly like this:\n{said}"
    );
}
