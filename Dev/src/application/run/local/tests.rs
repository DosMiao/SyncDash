use super::super::roots::resolve_root;
use super::compare::{escalate_sampled_disagreements, schedule_scans};
use super::*;
use crate::fs::vfs::memory::MemVfs;
use crate::fs::vfs::Vfs;
use crate::job::Job;
use crate::model::digest::Blake3Digest;
use crate::model::event::{Phase, PhaseStatus, ProgressEvent};
use crate::model::plan::{Action, Op, Plan};
use crate::model::table::TableArtifact;
use crate::model::table::{FileIdentityObservation, TableEvidence};
use crate::obs::progress::{PhaseProgress, RunCtl, RunCtx};
use crate::pipeline::{compare, scan};
use std::sync::{Arc, Mutex};

fn escalation_fixture(
    tag: &str,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    TableArtifact,
    TableArtifact,
    Plan,
    Job,
) {
    let base =
        std::env::temp_dir().join(format!("syncdash-escalation-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let source = base.join("source");
    let target = base.join("target");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(source.join("suspect.bin"), b"same").unwrap();
    std::fs::write(target.join("suspect.bin"), b"same").unwrap();

    let scan_one = |root: &std::path::Path| {
        scan::scan(
            root,
            &scan::ScanOptions {
                hash: false,
                sampled: false,
                use_cache: false,
                symlinks_direct: false,
                filter: crate::pipeline::filter::PathFilter::build(&[], &[]),
            },
        )
        .unwrap()
    };
    let mut source_snapshot = scan_one(&source);
    let mut target_snapshot = scan_one(&target);
    let sampled_digest = Blake3Digest::hash_bytes(b"same-sample");
    let source_entry = source_snapshot
        .entries
        .iter_mut()
        .find(|entry| entry.path().as_str() == "suspect.bin")
        .and_then(|entry| entry.as_file_mut())
        .unwrap();
    source_entry.identity = FileIdentityObservation::SampledBlake3 {
        digest: sampled_digest.clone(),
    };
    source_entry.mtime_ms = 10_000;
    let target_entry = target_snapshot
        .entries
        .iter_mut()
        .find(|entry| entry.path().as_str() == "suspect.bin")
        .and_then(|entry| entry.as_file_mut())
        .unwrap();
    target_entry.identity = FileIdentityObservation::SampledBlake3 {
        digest: sampled_digest,
    };
    target_entry.mtime_ms = 0;
    source_snapshot.header.evidence = TableEvidence::Sampled;
    target_snapshot.header.evidence = TableEvidence::Sampled;

    let mut job = Job::default();
    job.mode = "mirror".into();
    job.rigor = "fast".into();
    let plan = compare::compare(
        &source_snapshot,
        &target_snapshot,
        &job.mode,
        None,
        false,
        &job.compare_opts(),
    );
    assert!(
        plan.ops.is_empty(),
        "the sampled evidence alone calls this pair identical"
    );
    (source, target, source_snapshot, target_snapshot, plan, job)
}

#[test]
fn same_volume_scan_stops_before_target_when_source_fails() {
    let target_called = std::sync::atomic::AtomicBool::new(false);
    let result: std::io::Result<((), ())> = schedule_scans(
        true,
        || {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "source failed",
            ))
        },
        || {
            target_called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
    );

    assert_eq!(result.unwrap_err().to_string(), "source failed");
    assert!(!target_called.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn escalation_read_failure_aborts_instead_of_retaining_identical() {
    let (source, target, mut source_snapshot, mut target_snapshot, plan, job) =
        escalation_fixture("read-failure");
    std::fs::remove_file(source.join("suspect.bin")).unwrap();
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let copy = events.clone();
    let ctx = RunCtx::new(
        RunCtl::new(),
        Arc::new(move |event| copy.lock().unwrap().push(event)),
    );
    let pp = PhaseProgress::begin(&ctx, Phase::Compare, None, 0, 0);
    let source_vfs =
        Arc::new(crate::fs::vfs::local::LocalVfs::open(source.clone()).unwrap()) as Arc<dyn Vfs>;
    let target_vfs =
        Arc::new(crate::fs::vfs::local::LocalVfs::open(target.clone()).unwrap()) as Arc<dyn Vfs>;

    let error = match escalate_sampled_disagreements(
        &job,
        plan,
        &mut source_snapshot,
        &mut target_snapshot,
        &ctx,
        &source_vfs,
        &target_vfs,
        &pp,
    ) {
        Ok(_) => panic!("an unreadable full-verification file must abort comparison"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        error
            .to_string()
            .contains("cannot fully verify source 'suspect.bin'"),
        "{error}"
    );
    drop(pp);

    let events = events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        ProgressEvent::Totals {
            phase: Phase::Compare,
            items_total: 1,
            bytes_total: 8,
            ..
        }
    )));
    assert!(matches!(
        events.last(),
        Some(ProgressEvent::PhaseEnd {
            phase: Phase::Compare,
            status: PhaseStatus::Failed,
            ..
        })
    ));
    let _ = std::fs::remove_dir_all(source.parent().unwrap());
}

#[test]
fn escalation_honors_cancellation_before_reopening_files() {
    let (source, target, mut source_snapshot, mut target_snapshot, plan, job) =
        escalation_fixture("cancel");
    let ctl = RunCtl::new();
    ctl.request_cancel();
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let copy = events.clone();
    let ctx = RunCtx::new(ctl, Arc::new(move |event| copy.lock().unwrap().push(event)));
    let pp = PhaseProgress::begin(&ctx, Phase::Compare, None, 0, 0);
    let source_vfs =
        Arc::new(crate::fs::vfs::local::LocalVfs::open(source.clone()).unwrap()) as Arc<dyn Vfs>;
    let target_vfs =
        Arc::new(crate::fs::vfs::local::LocalVfs::open(target.clone()).unwrap()) as Arc<dyn Vfs>;

    let error = match escalate_sampled_disagreements(
        &job,
        plan,
        &mut source_snapshot,
        &mut target_snapshot,
        &ctx,
        &source_vfs,
        &target_vfs,
        &pp,
    ) {
        Ok(_) => panic!("cancelled escalation must not complete"),
        Err(error) => error,
    };
    assert!(crate::obs::progress::is_cancelled(&error));
    drop(pp);
    assert!(matches!(
        events.lock().unwrap().last(),
        Some(ProgressEvent::PhaseEnd {
            phase: Phase::Compare,
            status: PhaseStatus::Cancelled,
            ..
        })
    ));
    let _ = std::fs::remove_dir_all(source.parent().unwrap());
}

#[test]
fn escalation_rechecks_nonlocal_vfs_roots_instead_of_skipping_them() {
    let source = MemVfs::new("escalate-vfs-source").without(|caps| caps.max_parallel_streams = 1);
    let target = MemVfs::new("escalate-vfs-target").without(|caps| caps.max_parallel_streams = 1);
    source.seed_bytes("suspect.bin", b"src!", 10_000);
    target.seed_bytes("suspect.bin", b"tgt!", 0);
    let source = Arc::new(source) as Arc<dyn Vfs>;
    let target = Arc::new(target) as Arc<dyn Vfs>;
    let opt = scan::ScanOptions {
        hash: false,
        sampled: false,
        use_cache: false,
        symlinks_direct: false,
        filter: crate::pipeline::filter::PathFilter::build(&[], &[]),
    };
    let mut source_snapshot =
        scan::scan_root(&source, &opt, &RunCtx::null(), Phase::ScanSource).unwrap();
    let mut target_snapshot =
        scan::scan_root(&target, &opt, &RunCtx::null(), Phase::ScanTarget).unwrap();
    let sampled_digest = Blake3Digest::hash_bytes(b"same-sample");
    source_snapshot.entries[0].as_file_mut().unwrap().identity =
        FileIdentityObservation::SampledBlake3 {
            digest: sampled_digest.clone(),
        };
    target_snapshot.entries[0].as_file_mut().unwrap().identity =
        FileIdentityObservation::SampledBlake3 {
            digest: sampled_digest,
        };
    source_snapshot.header.evidence = TableEvidence::Sampled;
    target_snapshot.header.evidence = TableEvidence::Sampled;
    let mut job = Job::default();
    job.mode = "mirror".into();
    job.rigor = "fast".into();
    let plan = compare::compare(
        &source_snapshot,
        &target_snapshot,
        &job.mode,
        None,
        false,
        &job.compare_opts(),
    );
    assert!(plan.ops.is_empty());
    let ctx = RunCtx::null();
    let pp = PhaseProgress::begin(&ctx, Phase::Compare, None, 0, 0);

    let plan = escalate_sampled_disagreements(
        &job,
        plan,
        &mut source_snapshot,
        &mut target_snapshot,
        &ctx,
        &source,
        &target,
        &pp,
    )
    .unwrap();
    pp.finish().unwrap();

    assert_eq!(plan.ops.len(), 1);
    assert_eq!(plan.ops[0].action, Action::Update);
    assert!(plan.ops[0].reason.starts_with("escalated:"));
    let evidence = compare::evidence::evidence(
        &source_snapshot,
        &target_snapshot,
        &plan,
        &job.compare_opts(),
    );
    assert_eq!(evidence.identical_count, 0);
}

/// The generic VFS lane end to end: neither side exposes a retained local root, so this drives
/// `scan_vfs` rather than the local fast path, then compares and plans.
#[test]
fn vfs_lane_compares_and_classifies_every_drift() {
    let sv = MemVfs::new("cmp-src");
    let tv = MemVfs::new("cmp-tgt");
    // byte-identical on both sides — must produce no op at all
    sv.seed_file("a/same.bin", 10_000, 1_000_000);
    tv.seed_file("a/same.bin", 10_000, 1_000_000);
    // source-only -> copy; target-only -> delete (mirror)
    sv.seed_file("a/new.bin", 5_000, 1_000_000);
    tv.seed_file("a/gone.bin", 7_000, 1_000_000);
    // same path, different size -> update
    sv.seed_file("a/changed.bin", 9_000, 2_000_000);
    tv.seed_file("a/changed.bin", 8_000, 1_000_000);
    // an excluded directory on both sides must be pruned AND counted
    sv.seed_file("skipme/x.bin", 100, 0);
    tv.seed_file("skipme/x.bin", 100, 0);

    let mut j = Job::default();
    j.mode = "mirror".into();
    j.rigor = "standard".into();
    j.exclude = vec!["skipme/".into()];
    let (sv, tv) = (Arc::new(sv) as Arc<dyn Vfs>, Arc::new(tv) as Arc<dyn Vfs>);
    let out = compare_resolved(&j, &sv, &tv, &RunCtx::null(), false).unwrap();

    assert_eq!(
        out.source.header.excluded_dirs, 1,
        "a pruned subtree must be counted, never silent"
    );
    assert_eq!(out.target.header.excluded_dirs, 1);
    assert!(
        out.source.header.vfs.is_some(),
        "a VFS root's snapshot must carry its self-description"
    );
    assert!(
        !out.source
            .entries
            .iter()
            .any(|entry| entry.path().as_str().starts_with("skipme")),
        "pruned content must not enter the table"
    );

    let mut kinds: Vec<(String, String)> = out
        .plan
        .ops
        .iter()
        .map(|o| (format!("{:?}", o.action).to_lowercase(), o.path.clone()))
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        vec![
            ("copy".to_string(), "a/new.bin".to_string()),
            ("delete".to_string(), "a/gone.bin".to_string()),
            ("update".to_string(), "a/changed.bin".to_string()),
        ],
        "same.bin must compare equal; the three drifts must each classify"
    );
}

#[test]
fn preflight_uses_the_open_vfs_roots_instead_of_display_paths() {
    let source = MemVfs::new("preflight-source");
    let target = MemVfs::new("preflight-target");
    let marker = serde_json::to_vec(&crate::pipeline::guard::marker::Marker {
        job: "preflight-test".into(),
        host: "test-host".into(),
        created_at_ms: 1,
        note: String::new(),
    })
    .unwrap();
    source.seed_bytes(crate::foundation::names::MARKER_NAME, &marker, 1);
    target.seed_bytes(crate::foundation::names::MARKER_NAME, &marker, 1);
    let (source, target) = (
        Arc::new(source) as Arc<dyn Vfs>,
        Arc::new(target) as Arc<dyn Vfs>,
    );
    let job = Job {
        require_marker: true,
        ..Default::default()
    };
    let plan = compare_resolved(&job, &source, &target, &RunCtx::null(), false)
        .unwrap()
        .plan;

    let verdict = preflight_resolved(&job, &plan, &[], false, &source, &target);

    assert!(
        verdict.ok(),
        "VFS markers must satisfy preflight: {:?}",
        verdict.blockers
    );
}

#[test]
fn preflight_rejects_a_plan_for_different_resolved_roots() {
    let source = Arc::new(MemVfs::new("preflight-source")) as Arc<dyn Vfs>;
    let target = Arc::new(MemVfs::new("preflight-target")) as Arc<dyn Vfs>;
    let job = Job::default();
    let mut plan = compare_resolved(&job, &source, &target, &RunCtx::null(), false)
        .unwrap()
        .plan;
    plan.header.target_root = "mem://another-target".into();

    let verdict = preflight_resolved(&job, &plan, &[], false, &source, &target);

    assert!(!verdict.ok());
    assert!(verdict.blockers[0].contains("run Compare again"));

    let execution = apply_resolved_with_consent_classified(
        &job,
        &plan,
        &[],
        &source,
        &target,
        None,
        false,
        false,
        &crate::pipeline::guard::caps::CapabilityConsent::None,
        std::time::Instant::now(),
        &RunCtx::null(),
    );
    assert!(
        !execution.writes_started(),
        "a changed root label is rejected before the write lane"
    );
    assert_eq!(execution.into_result().unwrap().errors, 1);
}

#[test]
fn capability_and_health_refusals_are_classified_before_write() {
    let source = Arc::new(MemVfs::new("gate-source")) as Arc<dyn Vfs>;
    let target = Arc::new(MemVfs::new("gate-target").without(|caps| {
        caps.symlink = crate::fs::vfs::Support::No;
    })) as Arc<dyn Vfs>;
    let job = Job::default();
    let mut plan = compare_resolved(&job, &source, &target, &RunCtx::null(), false)
        .unwrap()
        .plan;
    let symlink = Op {
        side: crate::model::plan::Side::Target,
        action: Action::Copy,
        path: "link".into(),
        from: None,
        size: None,
        mtime_ms: None,
        hash: None,
        link: Some("destination".into()),
        mode: None,
        reason: "test capability boundary".into(),
    };
    plan.ops.push(symlink.clone());
    let capability_refusal = apply_resolved_with_consent_classified(
        &job,
        &plan,
        &[symlink],
        &source,
        &target,
        None,
        false,
        false,
        &crate::pipeline::guard::caps::CapabilityConsent::ExplicitCli,
        std::time::Instant::now(),
        &RunCtx::null(),
    );
    assert!(!capability_refusal.writes_started());
    assert_eq!(capability_refusal.into_result().unwrap().errors, 1);

    let source = Arc::new(MemVfs::new("health-source")) as Arc<dyn Vfs>;
    let target = Arc::new(MemVfs::new("health-target")) as Arc<dyn Vfs>;
    let healthy_job = Job::default();
    let plan = compare_resolved(&healthy_job, &source, &target, &RunCtx::null(), false)
        .unwrap()
        .plan;
    let mut marker_required = healthy_job;
    marker_required.require_marker = true;
    let health_refusal = apply_resolved_with_consent_classified(
        &marker_required,
        &plan,
        &[],
        &source,
        &target,
        None,
        false,
        false,
        &crate::pipeline::guard::caps::CapabilityConsent::ExplicitCli,
        std::time::Instant::now(),
        &RunCtx::null(),
    );
    assert!(!health_refusal.writes_started());
    assert_eq!(health_refusal.into_result().unwrap().errors, 1);
}

#[test]
fn entering_the_local_write_lane_is_never_a_safe_rejection() {
    let source_mem = MemVfs::new("write-source");
    source_mem.seed_bytes("new.txt", b"content", 1_000);
    let target_mem = MemVfs::new("write-target");
    let source = Arc::new(source_mem) as Arc<dyn Vfs>;
    let target = Arc::new(target_mem) as Arc<dyn Vfs>;
    let job = Job::default();
    let plan = compare_resolved(&job, &source, &target, &RunCtx::null(), false)
        .unwrap()
        .plan;
    assert_eq!(plan.ops.len(), 1);

    let execution = apply_resolved_with_consent_classified(
        &job,
        &plan,
        &plan.ops,
        &source,
        &target,
        None,
        false,
        false,
        &crate::pipeline::guard::caps::CapabilityConsent::ExplicitCli,
        std::time::Instant::now(),
        &RunCtx::null(),
    );
    assert!(execution.writes_started());
    let outcome = execution.into_result().unwrap();
    assert_eq!(outcome.errors, 0);
    assert_eq!(outcome.done, 1);
}

/// A backend that cannot serve ranged reads degrades the sampled evidence tier. That must
/// cost an explicit consent, and the consented degradation must ride on the snapshot.
#[test]
fn degraded_caps_demand_consent_and_land_on_the_table() {
    let sv = MemVfs::new("ack-src");
    let tv = MemVfs::new("ack-tgt").without(|c| c.ranged_read = crate::fs::vfs::Support::No);
    // Big enough that the sampled tier would sample, identical on both sides
    sv.seed_file("big.bin", 5 * 1024 * 1024, 1_000);
    tv.seed_file("big.bin", 5 * 1024 * 1024, 1_000);
    let mut j = Job::default();
    j.mode = "mirror".into();
    j.rigor = "fast".into(); // the sampled tier
    let (sv, tv) = (Arc::new(sv) as Arc<dyn Vfs>, Arc::new(tv) as Arc<dyn Vfs>);

    let e = match compare_resolved(&j, &sv, &tv, &RunCtx::null(), false) {
        Err(e) => e,
        Ok(_) => panic!("a degraded run must refuse without consent"),
    };
    assert!(e.to_string().contains("--accept-caps"), "{e}");

    // With consent: BOTH sides upgrade to full — a one-sided upgrade would make the
    // identical file look different — and the plan stays empty.
    let out = compare_resolved(&j, &sv, &tv, &RunCtx::null(), true).unwrap();
    assert_eq!(out.source.header.evidence, TableEvidence::Full);
    assert_eq!(out.target.header.evidence, TableEvidence::Full);
    assert!(
        !out.target.header.vfs.as_ref().unwrap().degraded.is_empty(),
        "the consented degradation must ride on the snapshot"
    );
    assert_eq!(
        out.plan.ops.len(),
        0,
        "identical content must not produce ops after the joint upgrade"
    );
}

/// An unknown scheme is a hard error at resolution, never a silent local path.
#[test]
fn resolve_refuses_an_unknown_scheme() {
    let e = match resolve_root("sfpt://typo/data") {
        Err(e) => e,
        Ok(_) => panic!("an unknown scheme must not resolve"),
    };
    assert!(e.to_string().contains("unknown scheme"), "{e}");
}

#[test]
fn apply_preflight_refusal_emits_exactly_one_terminal_summary() {
    let sv = Arc::new(MemVfs::new("terminal-src")) as Arc<dyn Vfs>;
    let tv = Arc::new(MemVfs::new("terminal-tgt")) as Arc<dyn Vfs>;
    let mut job = Job::default();
    job.source = "/definitely/missing/terminal-source".into();
    job.targets = vec!["/definitely/missing/terminal-target".into()];
    let plan = compare_resolved(&job, &sv, &tv, &RunCtx::null(), false)
        .unwrap()
        .plan;

    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let copy = events.clone();
    let ctx = RunCtx::new(
        RunCtl::new(),
        Arc::new(move |ev| copy.lock().unwrap().push(ev)),
    );
    let selected = job.select_target(0).unwrap();
    let execution = apply_job_guarded_with_consent_classified(
        &selected,
        &plan,
        &[],
        None,
        false,
        false,
        &crate::pipeline::guard::caps::CapabilityConsent::None,
        &ctx,
    );
    assert!(
        !execution.writes_started(),
        "a root that cannot open is a proven pre-write refusal"
    );
    let out = execution.into_result().unwrap();

    assert_eq!(out.errors, 1);
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Summary { .. }))
            .count(),
        1
    );
    assert!(matches!(
        events.last(),
        Some(ProgressEvent::Summary {
            errors: 1,
            cancelled: false,
            ..
        })
    ));
}

#[test]
fn terminal_summary_observes_a_last_moment_cancel_request() {
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let copy = events.clone();
    let ctl = RunCtl::new();
    ctl.request_cancel();
    let ctx = RunCtx::new(ctl, Arc::new(move |ev| copy.lock().unwrap().push(ev)));

    let out = crate::obs::progress::ApplyOutcome::default().finish(&ctx, std::time::Instant::now());
    assert!(out.cancelled);
    assert!(matches!(
        events.lock().unwrap().last(),
        Some(ProgressEvent::Summary {
            cancelled: true,
            ..
        })
    ));
}
