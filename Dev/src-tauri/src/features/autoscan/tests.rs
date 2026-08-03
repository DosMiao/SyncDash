use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Barrier, Mutex};

use crate::contracts::compare::{CompareOwner, CompareScopeExecutionStatusDto};
use crate::features::autoscan::authority::AutoScanComparePermit;
use crate::features::compare::evidence::model::result::SuccessfulCompareResult;
use crate::features::compare::evidence::model::verification::CompareVerificationTicket;
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::operations::authorization::store::OperationAuthorizationStore;

use super::worker::binding::validate_resolved_binding;
use super::worker::configuration::{configured_interval, next_ticket_id, DEFAULT_INTERVAL_SECS};
use super::worker::observation::{begin_observed_trigger, mark_shared_inactive};
use super::{controller::*, model::*, runtime::*, state::*};

fn binding() -> AutoScanBinding {
    AutoScanBinding {
        job_id: "job-id-photos".into(),
        job_name: "photos".into(),
        config_revision: "revision-a".into(),
        target_index: 1,
        interval_secs: 30,
        auto_apply: false,
        rigor: "standard".into(),
    }
}

#[test]
fn compare_ownership_includes_revision_and_target() {
    let binding = binding();
    let owner = CompareOwner {
        identity: crate::contracts::compare::CompareIdentity {
            result_id: "11111111111111111111111111111111".into(),
            compare_run_id: 7,
            job_id: "job-id-photos".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
        },
        job_name: "photos".into(),
    };
    assert!(binding.owns_compare(&owner));
    assert!(binding.owns_compare(&CompareOwner {
        job_name: "renamed".into(),
        ..owner.clone()
    }));
    let mut replaced = owner.clone();
    replaced.identity.job_id = "replacement-id".into();
    assert!(!binding.owns_compare(&replaced));
    let mut revised = owner.clone();
    revised.identity.config_revision = "revision-b".into();
    assert!(!binding.owns_compare(&revised));
    let mut retargeted = owner;
    retargeted.identity.target_index = 0;
    assert!(!binding.owns_compare(&retargeted));
}

#[test]
fn configured_interval_is_explicit_and_never_zero() {
    let mut job = syncdash::job::Job::default();
    assert_eq!(configured_interval(&job), DEFAULT_INTERVAL_SECS);
    job.autoscan_interval_secs = Some(0);
    assert_eq!(configured_interval(&job), 1);
    job.autoscan_interval_secs = Some(90);
    assert_eq!(configured_interval(&job), 90);
}

#[test]
fn counters_fail_closed_without_consuming_pending_work() {
    let counter = AtomicU64::new(u64::MAX - 1);
    assert_eq!(
        allocate_unique_id(&counter, "test identity").unwrap(),
        u64::MAX
    );
    assert!(allocate_unique_id(&counter, "test identity")
        .unwrap_err()
        .contains("ID space is exhausted"));
    assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    assert_eq!(next_ticket_id(u64::MAX - 1), Some(u64::MAX));
    assert_eq!(next_ticket_id(u64::MAX), None);

    let (controller, _receiver) = controller_waiting_for(29, false);
    controller.set_compare_permit_counter_for_test(u64::MAX);
    assert!(controller.issue_compare_permit(4, 29).is_err());
    assert_eq!(controller.status().active_ticket, Some(29));
    assert!(controller.decline_trigger(4, 29).is_ok());
}

fn trigger(binding: &AutoScanBinding, generation: u64, ticket_id: u64) -> AutoScanTriggerDto {
    AutoScanTriggerDto {
        generation,
        ticket_id,
        job_id: binding.job_id.clone(),
        job_name: binding.job_name.clone(),
        config_revision: binding.config_revision.clone(),
        target_index: binding.target_index,
        auto_apply: binding.auto_apply,
        mode: AutoScanDetectionMode::Polling,
        reason: AutoScanTriggerReason::FilesystemChange,
    }
}

fn controller() -> AutoScanController {
    AutoScanController::new(
        Arc::new(CompareResultRepository::default()),
        Arc::new(OperationAuthorizationStore::default()),
    )
}

fn controller_waiting_for(
    ticket_id: u64,
    auto_apply: bool,
) -> (AutoScanController, mpsc::Receiver<WorkerCommand>) {
    let controller = controller();
    let mut binding = binding();
    binding.auto_apply = auto_apply;
    let (commands, receiver) = mpsc::channel();
    let mut status = AutoScanStatusCore::starting(4, &binding);
    status.latest_ticket_id = ticket_id;
    let pending_trigger = trigger(&binding, 4, ticket_id);
    let verification = controller
        .execution
        .results
        .begin_verification(binding.compare_scope(), None)
        .unwrap();
    *controller.active.lock().unwrap() = Some(ActiveAutoScan {
        binding,
        generation: 4,
        commands,
        shared: Arc::new(Mutex::new(AutoScanShared {
            status,
            ticket: AutoScanTicketLifecycle::AwaitingCompare {
                trigger: pending_trigger,
                verification,
            },
        })),
        events: AutoScanEvents::Suppressed,
        join: None,
    });
    (controller, receiver)
}

fn owner() -> CompareOwner {
    CompareOwner {
        identity: crate::contracts::compare::CompareIdentity {
            result_id: "22222222222222222222222222222222".into(),
            compare_run_id: 8,
            job_id: "job-id-photos".into(),
            config_revision: "revision-a".into(),
            target_index: 1,
        },
        job_name: "photos".into(),
    }
}

fn successful_result(owner: CompareOwner) -> SuccessfulCompareResult {
    let plan = crate::contracts::compare::PlanDto {
        owner,
        header: syncdash::model::plan::PlanHeader {
            schema: syncdash::model::plan::PLAN_SCHEMA,
            kind: "plan".into(),
            mode: "mirror".into(),
            generated_at_ms: 0,
            source_root: "/source".into(),
            source_host: "host".into(),
            target_root: "/target".into(),
            target_host: "host".into(),
            op_count: 0,
            conflict_count: 0,
            source_entries: 0,
            target_entries: 0,
            source_excluded: 0,
            target_excluded: 0,
            source_walk_errors: 0,
            target_walk_errors: 0,
            source_walk_err_samples: Vec::new(),
            target_walk_err_samples: Vec::new(),
            source_icloud_stubs: 0,
            target_icloud_stubs: 0,
            source_icloud_stub_samples: Vec::new(),
            target_icloud_stub_samples: Vec::new(),
        },
        ops: Vec::new(),
        metas: Vec::new(),
        identical_count: 0,
        identical_bytes: 0,
        mtime_window_ms: syncdash::pipeline::compare::MTIME_SLACK_MS,
    };
    let snapshot = |root: &str| syncdash::model::table::TableArtifact {
        header: syncdash::model::table::TableHeader {
            schema: syncdash::model::table::TABLE_SCHEMA,
            kind: syncdash::model::table::TableKind::Snapshot,
            root: root.into(),
            host: "host".into(),
            os: "test".into(),
            scanned_at_ms: 0,
            duration_ms: 0,
            entry_count: 0,
            evidence: syncdash::model::table::TableEvidence::None,
            excluded_dirs: 0,
            excluded_files: 0,
            walk_errors: 0,
            walk_err_samples: Vec::new(),
            icloud_stubs: 0,
            icloud_stub_samples: Vec::new(),
            skipped_symlinks: 0,
            dataless_files: 0,
            vfs: None,
        },
        entries: Vec::new(),
    };
    SuccessfulCompareResult::from_plan(
        "test-plan-digest".into(),
        plan,
        snapshot("/source"),
        snapshot("/target"),
        syncdash::pipeline::compare::CompareOptions::default(),
    )
}

fn publish_current_compare(controller: &AutoScanController) -> AutoScanComparePublication {
    let status = controller.status();
    let ticket_id = status.active_ticket.unwrap();
    let permit = controller.issue_compare_permit(4, ticket_id).unwrap();
    let owner = owner();
    controller
        .mark_compare_launched(&permit, owner.identity.compare_run_id)
        .unwrap();
    controller
        .publish_successful_compare(&permit, successful_result(owner))
        .unwrap()
}

fn pending_verification(controller: &AutoScanController) -> CompareVerificationTicket {
    let shared = controller
        .active
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .shared
        .clone();
    let verification = match &shared.lock().unwrap().ticket {
        AutoScanTicketLifecycle::AwaitingCompare { verification, .. } => verification.clone(),
        _ => panic!("test controller must be awaiting Compare"),
    };
    verification
}

#[test]
fn decline_is_one_use_and_requires_the_exact_unlaunched_trigger() {
    let (controller, receiver) = controller_waiting_for(11, false);
    assert!(controller.decline_trigger(3, 11).is_err());
    assert!(controller.decline_trigger(4, 12).is_err());
    controller.issue_compare_permit(4, 11).unwrap();
    let status = controller.decline_trigger(4, 11).unwrap();
    assert_eq!(status.active_ticket, None);
    assert!(matches!(
        controller
            .execution
            .results
            .execution_status(&binding().compare_scope()),
        CompareScopeExecutionStatusDto::Failed { message, .. }
            if message == "AutoScan verification failed: The trigger was declined before Compare launched"
    ));
    assert!(controller.decline_trigger(4, 11).is_err());
    assert!(matches!(
        receiver.try_recv(),
        Ok(WorkerCommand::VerificationTerminated { ticket_id: 11 })
    ));
}

#[test]
fn observed_trigger_advances_verification_before_ticket_state_is_visible() {
    let (controller, _receiver) = controller_waiting_for(11, false);
    let previous = pending_verification(&controller);
    let binding = controller
        .active
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .binding
        .clone();

    let next = begin_observed_trigger(&binding, &controller.execution).unwrap();

    assert_ne!(next, previous);
    assert_eq!(pending_verification(&controller), previous);
}

#[test]
fn successful_publication_requires_the_authenticated_running_compare() {
    let (controller, receiver) = controller_waiting_for(12, true);
    let forged_manual_permit = AutoScanComparePermit::new(
        999,
        4,
        12,
        "job-id-photos".into(),
        "revision-a".into(),
        1,
        pending_verification(&controller),
    );
    assert!(controller
        .publish_successful_compare(&forged_manual_permit, successful_result(owner()),)
        .is_err());

    let permit = controller.issue_compare_permit(4, 12).unwrap();
    let mut wrong = owner();
    wrong.identity.target_index = 0;
    assert!(controller
        .publish_successful_compare(&permit, successful_result(wrong))
        .is_err());
    assert!(receiver.try_recv().is_err());
    controller
        .mark_compare_launched(&permit, owner().identity.compare_run_id)
        .unwrap();
    assert!(controller.mark_compare_launched(&permit, 99).is_err());
    let mut expected_owner = owner();
    expected_owner.job_name = "externally-renamed".into();
    let completed = controller
        .publish_successful_compare(&permit, successful_result(expected_owner))
        .unwrap();
    assert_eq!(completed.autoscan_status.active_ticket, None);
    assert_eq!(completed.autoscan_status.pending_trigger, None);
    assert_eq!(
        completed.autoscan_status.job_name.as_deref(),
        Some("externally-renamed")
    );
    assert!(matches!(
        receiver.try_recv(),
        Ok(WorkerCommand::VerificationPublished { ticket_id: 12 })
    ));
    let ticket = controller.claim_completed_auto_apply(4, 12).unwrap();
    assert_eq!(ticket.compare_identity(), &owner().identity);
}

#[test]
fn published_evidence_survives_worker_disconnect_but_autoapply_stops_fail_closed() {
    let (controller, receiver) = controller_waiting_for(32, true);
    drop(receiver);
    let permit = controller.issue_compare_permit(4, 32).unwrap();
    let expected_owner = owner();
    controller
        .mark_compare_launched(&permit, expected_owner.identity.compare_run_id)
        .unwrap();

    let completed = controller
        .publish_successful_compare(&permit, successful_result(expected_owner.clone()))
        .unwrap();

    assert!(!completed.autoscan_status.active);
    assert_eq!(completed.autoscan_status.active_ticket, None);
    assert!(controller
        .execution
        .results
        .get_exact(&expected_owner.identity)
        .unwrap()
        .is_some());
    assert!(controller.claim_completed_auto_apply(4, 32).is_err());
}

#[test]
fn abandoned_permitted_compare_terminalizes_and_releases_its_ticket() {
    let (controller, receiver) = controller_waiting_for(27, false);
    let permit = controller.issue_compare_permit(4, 27).unwrap();
    let status = controller
        .terminalize_permitted_verification(
            &permit,
            AutoScanVerificationTerminal::Failed("review was abandoned".into()),
        )
        .unwrap();
    assert_eq!(status.active_ticket, None);
    assert_eq!(status.pending_trigger, None);
    assert!(matches!(
        controller
            .execution
            .results
            .execution_status(&binding().compare_scope()),
        CompareScopeExecutionStatusDto::Failed { message, .. }
            if message == "AutoScan verification failed: review was abandoned"
    ));
    assert!(matches!(
        receiver.try_recv(),
        Ok(WorkerCommand::VerificationTerminated { ticket_id: 27 })
    ));
    assert!(controller.issue_compare_permit(4, 27).is_err());
}

#[test]
fn ui_decline_cannot_terminalize_a_compare_after_backend_launch() {
    let (controller, receiver) = controller_waiting_for(31, false);
    let permit = controller.issue_compare_permit(4, 31).unwrap();
    controller
        .mark_compare_launched(&permit, owner().identity.compare_run_id)
        .unwrap();

    assert!(controller.decline_trigger(4, 31).is_err());
    assert_eq!(controller.status().active_ticket, Some(31));
    assert!(receiver.try_recv().is_err());

    controller
        .terminalize_permitted_verification(&permit, AutoScanVerificationTerminal::Cancelled)
        .unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Ok(WorkerCommand::VerificationTerminated { ticket_id: 31 })
    ));
}

#[test]
fn stopped_generation_rejects_publication_before_repository_transition() {
    let (controller, _receiver) = controller_waiting_for(28, false);
    let permit = controller.issue_compare_permit(4, 28).unwrap();
    controller
        .mark_compare_launched(&permit, owner().identity.compare_run_id)
        .unwrap();
    controller.stop();
    let expected_owner = owner();
    assert!(controller
        .publish_successful_compare(&permit, successful_result(expected_owner.clone()),)
        .is_err());
    assert!(controller
        .execution
        .results
        .get_exact(&expected_owner.identity)
        .unwrap()
        .is_none());
}

#[test]
fn status_query_recovers_the_full_trigger_when_event_delivery_is_lost() {
    let (controller, _receiver) = controller_waiting_for(14, true);
    let status = controller.status();
    let pending = status
        .pending_trigger
        .expect("the server-owned trigger must survive independently of event delivery");
    assert_eq!(status.latest_ticket_id, 14);
    assert_eq!(pending.generation, 4);
    assert_eq!(pending.ticket_id, 14);
    assert_eq!(pending.job_id, "job-id-photos");
    assert_eq!(pending.config_revision, "revision-a");
    assert_eq!(pending.target_index, 1);
    assert!(pending.auto_apply);
}

#[test]
fn completed_autoapply_ticket_is_claimed_authorized_and_consumed_exactly_once() {
    let (controller, _receiver) = controller_waiting_for(15, true);
    let status = publish_current_compare(&controller).autoscan_status;
    assert_eq!(status.latest_ticket_id, 15);
    assert_eq!(status.active_ticket, None);
    assert_eq!(status.pending_trigger, None);

    assert!(controller.claim_completed_auto_apply(3, 15).is_err());
    assert!(controller.claim_completed_auto_apply(4, 14).is_err());
    let ticket = controller.claim_completed_auto_apply(4, 15).unwrap();
    assert!(controller.claim_completed_auto_apply(4, 15).is_err());
    assert_eq!(
        controller
            .authorize_claim(&ticket, || Ok::<_, String>("token"))
            .unwrap(),
        "token"
    );
    assert_eq!(
        controller
            .consume_authorized_with(&ticket, || Ok::<_, String>("reserved"))
            .unwrap(),
        "reserved"
    );
    assert!(controller
        .consume_authorized_with(&ticket, || Ok::<_, String>(()))
        .is_err());
}

#[test]
fn completed_autoapply_claim_has_exactly_one_concurrent_winner() {
    let (controller, _receiver) = controller_waiting_for(20, true);
    publish_current_compare(&controller);
    let controller = Arc::new(controller);
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let controller = controller.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            controller.claim_completed_auto_apply(4, 20).is_ok()
        }));
    }
    barrier.wait();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
}

#[test]
fn failed_authorization_or_reservation_cannot_replay_the_ticket() {
    let (controller, _receiver) = controller_waiting_for(21, true);
    publish_current_compare(&controller);
    let ticket = controller.claim_completed_auto_apply(4, 21).unwrap();
    assert!(controller
        .authorize_claim(&ticket, || Err::<(), _>("grant disappeared".into()))
        .is_err());
    assert!(controller.claim_completed_auto_apply(4, 21).is_err());
    assert!(controller
        .authorize_claim(&ticket, || Ok::<_, String>(()))
        .is_err());

    let (controller, _receiver) = controller_waiting_for(22, true);
    publish_current_compare(&controller);
    let ticket = controller.claim_completed_auto_apply(4, 22).unwrap();
    controller
        .authorize_claim(&ticket, || Ok::<_, String>(()))
        .unwrap();
    assert!(controller
        .consume_authorized_with(&ticket, || Err::<(), _>("reservation lost".into()))
        .is_err());
    assert!(controller
        .consume_authorized_with(&ticket, || Ok::<_, String>(()))
        .is_err());
}

#[test]
fn decline_or_non_autoapply_publication_never_creates_write_authority() {
    let (failed, _receiver) = controller_waiting_for(16, true);
    failed.decline_trigger(4, 16).unwrap();
    assert!(failed.claim_completed_auto_apply(4, 16).is_err());

    let (manual, _receiver) = controller_waiting_for(17, false);
    publish_current_compare(&manual);
    assert!(manual.claim_completed_auto_apply(4, 17).is_err());
}

#[test]
fn stop_discards_a_claim_and_rename_relabels_without_invalidating_it() {
    let (controller, _receiver) = controller_waiting_for(18, true);
    publish_current_compare(&controller);
    controller
        .rebind_job_name("job-id-photos", "renamed-after-publication")
        .unwrap();
    let ticket = controller.claim_completed_auto_apply(4, 18).unwrap();
    assert_eq!(ticket.compare_identity(), &owner().identity);
    let stopped = controller.stop();
    assert!(!stopped.active);
    assert_eq!(stopped.generation, 4);
    assert_eq!(stopped.latest_ticket_id, 18);
    assert_eq!(controller.status(), stopped);
    assert!(controller
        .authorize_claim(&ticket, || Ok::<_, String>(()))
        .is_err());
}

#[test]
fn job_mutation_stops_only_the_monitored_identity_and_reports_canonical_inactive() {
    let never_started = controller().status();
    assert!(!never_started.active);
    assert_eq!(never_started.generation, 0);
    assert_eq!(never_started.latest_ticket_id, 0);

    let (controller, _receiver) = controller_waiting_for(23, true);
    assert!(controller.stop_if_job_id("unrelated-job").is_none());
    assert!(controller.status().active);
    assert_eq!(controller.status().active_ticket, Some(23));

    let inactive = controller
        .stop_if_job_id("job-id-photos")
        .expect("the monitored identity must be stopped");
    assert!(!inactive.active);
    assert_eq!(inactive.generation, 4);
    assert_eq!(inactive.latest_ticket_id, 23);
    assert_eq!(inactive.job_id.as_deref(), Some("job-id-photos"));
    assert_eq!(inactive.pending_trigger, None);
    assert_eq!(controller.status(), inactive);
    assert!(matches!(
        controller
            .execution
            .results
            .execution_status(&binding().compare_scope()),
        CompareScopeExecutionStatusDto::Cancelled { .. }
    ));
}

#[test]
fn worker_failure_retains_an_orderable_recoverable_tombstone() {
    let (controller, _receiver) = controller_waiting_for(26, true);
    let shared = controller
        .active
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .shared
        .clone();
    let stopped = mark_shared_inactive(&mut shared.lock().unwrap(), "registry changed");
    assert!(!stopped.active);
    assert_eq!(stopped.generation, 4);
    assert_eq!(stopped.latest_ticket_id, 26);
    assert_eq!(stopped.job_id.as_deref(), Some("job-id-photos"));
    assert_eq!(stopped.active_ticket, None);
    assert_eq!(stopped.pending_trigger, None);
    assert_eq!(controller.status(), stopped);
}

#[test]
fn concurrent_duplicate_declines_have_exactly_one_winner() {
    let (controller, receiver) = controller_waiting_for(19, false);
    let controller = Arc::new(controller);
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let controller = controller.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            controller.decline_trigger(4, 19).is_ok()
        }));
    }
    barrier.wait();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
    assert!(matches!(
        receiver.try_recv(),
        Ok(WorkerCommand::VerificationTerminated { ticket_id: 19 })
    ));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn pure_rename_rebinds_status_without_consuming_generation_or_ticket() {
    let (controller, _receiver) = controller_waiting_for(13, false);
    assert!(controller
        .rebind_job_name("replacement-id", "ignored")
        .is_none());

    let status = controller
        .rebind_job_name("job-id-photos", "renamed")
        .expect("the active identity should be rebound");
    assert_eq!(status.generation, 4);
    assert_eq!(status.latest_ticket_id, 13);
    assert_eq!(status.active_ticket, Some(13));
    assert_eq!(status.job_id.as_deref(), Some("job-id-photos"));
    assert_eq!(status.job_name.as_deref(), Some("renamed"));
    assert_eq!(
        status
            .pending_trigger
            .as_ref()
            .map(|trigger| trigger.job_name.as_str()),
        Some("renamed")
    );
    assert_eq!(
        controller
            .active
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .binding
            .job_name,
        "renamed"
    );
}

#[test]
fn resolved_binding_accepts_rename_but_rejects_same_name_replacement() {
    let mut registered = syncdash::job::Job {
        job_id: "job-id-original".into(),
        ..Default::default()
    };
    let revision = syncdash::job::config_revision(&registered).unwrap();
    let mut binding = binding();
    binding.job_id = registered.job_id.clone();
    binding.job_name = "old-name".into();
    binding.config_revision = revision.clone();
    binding.target_index = 0;

    validate_resolved_binding(&binding, "renamed", &registered).unwrap();

    registered.job_id = "job-id-replacement".into();
    let error = validate_resolved_binding(&binding, "old-name", &registered).unwrap_err();
    assert!(error.contains("replacement identity"), "{error}");
}
