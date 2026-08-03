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

/// Four root files keep the delete-ratio denominator free of directory entries.
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
];

/// Both roots hold `FLAT`; the source keeps one, producing a 75% delete plan.
fn mass_delete_pair() -> (Arc<dyn Vfs>, Arc<dyn Vfs>) {
    let sv: Arc<dyn Vfs> = Arc::new(MemVfs::new("guard-src"));
    let tv: Arc<dyn Vfs> = Arc::new(MemVfs::new("guard-tgt"));
    corpus::seed_into(&sv, FLAT);
    corpus::seed_into(&tv, FLAT);
    for s in &FLAT[1..] {
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

/// A mass deletion is reported, loudly and with its numbers, and then it runs. A wrong filter and a
/// real mass delete are indistinguishable from inside the plan, so the engine states the share and
/// leaves the call to the operator reading it rather than withholding their data.
#[test]
fn a_mass_delete_is_reported_in_full_and_then_runs() {
    let (sv, tv) = mass_delete_pair();
    let (plan, ap, said) = try_cycle(&guarded_job(0.5, false), &sv, &tv);

    assert_eq!(
        plan.ops.len(),
        3,
        "the plan really does propose all three deletions"
    );
    assert_eq!(
        ap.errors, 0,
        "a deletion share must not refuse the run\n{said}"
    );
    assert!(
        ap.done >= 3,
        "every proposed deletion should have run, got {}",
        ap.done
    );
    assert!(
        said.contains("over the 50% mark"),
        "the run has to say what tripped and why:\n{said}"
    );
    let tol = tree::Tolerance::between(&sv, &tv);
    tree::assert_same(
        &tree::shape_of(&sv),
        &tree::shape_of(&tv),
        &tol,
        "after a reported mass delete",
    );
}

/// The marker gate fires at *compare*, before a plan exists. It is a precondition, not a judgment
/// call: a root without its marker is a root that may not be mounted, and there is nothing to weigh.
#[test]
fn a_missing_mount_marker_still_refuses_before_any_plan_exists() {
    let (sv, tv) = mass_delete_pair();
    let (_, ctx) = watched();
    // `expect_err` would need `CompareOutcome: Debug`; a shipped type does not grow a derive to
    // suit a test.
    let Err(err) = crate::run::local::compare_resolved(&guarded_job(0.0, true), &sv, &tv, &ctx)
    else {
        panic!("a missing marker must refuse");
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
    let (_, ap, said) = try_cycle(&guarded_job(0.0, false), &sv, &tv);
    assert_eq!(
        ap.errors, 0,
        "an empty target is filled, not refused\n{said}"
    );
    assert!(
        said.contains("empty and unmarked"),
        "but it must say so, because an unmounted share looks exactly like this:\n{said}"
    );
}
