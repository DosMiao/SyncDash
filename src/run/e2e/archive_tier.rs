//! The archive and the comparison have to agree on how the evidence was gathered.
//!
//! A sampled digest and a full digest occupy different observation variants. An archive written in one
//! tier and read in another therefore calls every file over the sampling floor "changed" — and a
//! file the far side merely deleted lands as `deleted-on-target-but-changed-on-source`, the one
//! conflict kind no `on_conflict` policy resolves. It never clears.
//!
//! Two independent triggers, so two tests: an asymmetric pair of backends *within* one run, and a
//! `rigor` change *between* runs. Deriving the tier once (`run::effective_scan_opts`) fixes the
//! first; only stamping the tier into the archive and checking it on the way back in can catch the
//! second.

use std::sync::Arc;

use super::*;
use crate::fs::vfs::memory::MemVfs;
use crate::fs::vfs::{Support, Vfs};

/// 6 MiB, so it is over the 4 MiB sampling floor and actually gets sampled evidence.
const BIG: Seed = Seed {
    path: "big/handbook.bin",
    seed: 20,
    size: 6 * 1_048_576,
    mtime_ms: 1_767_225_600_000,
};

fn archive_job(rigor: &str, at: &std::path::Path) -> Job {
    Job {
        mode: "sync".into(),
        rigor: rigor.into(),
        archive: Some(at.to_path_buf()),
        ..bare_job()
    }
}

fn scratch_archive(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("syncdash-e2e-{tag}-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// The pair that triggers it is ordinary: a source that can do ranged reads (any local disk) against
/// a target that cannot (FTP). The joint rule forces *both* sides of the comparison to full hashes —
/// but the archive refresh rescans only the source, which can still sample, so the archive fills
/// with `~` digests the next comparison can never match.
#[test]
fn the_archive_and_the_comparison_must_agree_on_evidence_tier() {
    let sv: Arc<dyn Vfs> = Arc::new(MemVfs::new("x1-src"));
    let tv: Arc<dyn Vfs> = Arc::new(MemVfs::new("x1-tgt").without(|c| c.ranged_read = Support::No));
    corpus::seed_into(&sv, &[BIG]);
    corpus::seed_into(&tv, &[BIG]);

    let arch = scratch_archive("x1");
    let job = archive_job("standard", &arch);

    // Round one. The two sides already agree, so this exists only to write the archive.
    let p1 = cycle(&job, &sv, &tv);
    assert!(
        p1.ops.is_empty(),
        "identical trees should need no work: {:?}",
        p1.ops
    );

    let archived = crate::run::archive::load_archive(&arch)
        .expect("archive readable")
        .expect("archive written");
    let row = archived
        .entries
        .iter()
        .find(|entry| entry.path().as_str() == BIG.path)
        .expect("the big file is in the archive");
    let archived_hash = row
        .as_file()
        .and_then(|file| file.identity.digest())
        .expect("archived with a digest");

    // Round two. The target drops the file. With a usable archive that reads as "they deleted it",
    // and the delete propagates back to the source.
    tv.remove_file(BIG.path).expect("remove on target");
    let (_, ctx) = watched();
    let out = crate::run::local::compare_resolved(&job, &sv, &tv, &ctx, true).expect("compare");

    let reasons: Vec<&str> = out.plan.ops.iter().map(|o| o.reason.as_str()).collect();
    assert!(
        !reasons
            .iter()
            .any(|r| r.starts_with("deleted-on-target-but-changed-on-source")),
        "the source never changed, so this must not be a delete-versus-edit conflict — and that \
         conflict is unresolvable by any policy, so it would never clear.\n\
         archive recorded {archived_hash:?} for a file the comparison re-read at the joint tier.\n\
         plan: {:?}",
        out.plan.ops
    );
    assert_eq!(
        reasons,
        vec!["deleted-on-target"],
        "a file the target deleted and the source left alone should propagate as a delete"
    );

    let _ = std::fs::remove_file(&arch);
}

/// The second way the tiers can disagree, which sharing the rule cannot prevent: the job's `rigor`
/// changes between runs. Yesterday's archive is then written in an evidence tier today's comparison
/// does not speak, and no amount of deriving the tier consistently *within* a run helps.
///
/// So the archive states its tier and the reader checks it. Refusing costs attribution — sync drops
/// to the documented safe mode: fills both ways, reports differences, deletes nothing — and that is
/// the right trade against manufacturing conflicts that never clear.
#[test]
fn an_archive_from_a_different_rigor_is_refused_not_misread() {
    let sv: Arc<dyn Vfs> = Arc::new(MemVfs::new("x1b-src"));
    let tv: Arc<dyn Vfs> = Arc::new(MemVfs::new("x1b-tgt"));
    corpus::seed_into(&sv, &[BIG]);
    corpus::seed_into(&tv, &[BIG]);

    let arch = scratch_archive("x1b");

    // Both roots sample happily, so `standard` writes a sampled archive.
    cycle(&archive_job("standard", &arch), &sv, &tv);
    let a = crate::run::archive::load_archive(&arch)
        .expect("archive readable")
        .expect("archive written");
    assert_eq!(
        a.header.evidence,
        crate::model::table::TableEvidence::Sampled,
        "the archive has to record the tier it was gathered in, or the reader has nothing to check"
    );

    // The user raises rigor. `paranoid` reads every byte, so nothing it produces can match.
    tv.remove_file(BIG.path).expect("remove on target");
    let (said, ctx) = watched();
    let out =
        crate::run::local::compare_resolved(&archive_job("paranoid", &arch), &sv, &tv, &ctx, true)
            .expect("compare");

    let text = said.text();
    assert!(
        text.contains("archive was written with sampled evidence")
            && text.contains("compares at full"),
        "the refusal has to be stated, not silent — a run that quietly stopped attributing \
         deletions would look identical to one that had no deletions:\n{text}"
    );
    assert!(
        !out.plan
            .ops
            .iter()
            .any(|o| o.reason.contains("deleted-on-target-but-changed-on-source")),
        "an unusable archive must not be read as evidence the source changed: {:?}",
        out.plan.ops
    );
    assert_eq!(
        out.plan
            .ops
            .iter()
            .map(|o| o.reason.as_str())
            .collect::<Vec<_>>(),
        vec!["only-in-source"],
        "with no usable archive, sync fills both ways and never deletes"
    );

    let _ = std::fs::remove_file(&arch);
}
