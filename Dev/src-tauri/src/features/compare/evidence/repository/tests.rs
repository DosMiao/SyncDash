use std::sync::Arc;

use crate::contracts::compare::{
    CompareExecutionExpiryReasonDto, CompareIdentity, CompareOwner, CompareScopeExecutionStatusDto,
    CompareWorkspaceLookupDto, PlanDto,
};

use super::super::model::error::*;
use super::super::model::execution::*;
use super::super::model::result::*;
use super::super::model::scope::*;
use super::super::model::verification::*;
use super::super::state::HOT_RESULT_CACHE_CAPACITY;
use super::validation::validate_retained_compare;
use super::*;
use syncdash::model::plan::PlanHeader;
use syncdash::model::table::{TableArtifact, TableEvidence, TableHeader, TableKind, TABLE_SCHEMA};

fn identity(
    job_id: &str,
    target_index: usize,
    revision: &str,
    compare_run_id: u64,
) -> CompareIdentity {
    let result_digest =
        blake3::hash(format!("{job_id}\0{target_index}\0{revision}\0{compare_run_id}").as_bytes())
            .to_hex()
            .to_string();
    CompareIdentity {
        result_id: result_digest[..32].to_string(),
        compare_run_id,
        job_id: job_id.into(),
        target_index,
        config_revision: revision.into(),
    }
}

fn owner(
    job_id: &str,
    job_name: &str,
    target_index: usize,
    revision: &str,
    compare_run_id: u64,
) -> CompareOwner {
    CompareOwner {
        identity: identity(job_id, target_index, revision, compare_run_id),
        job_name: job_name.into(),
    }
}

fn version(
    job_id: &str,
    job_name: &str,
    target_index: usize,
    revision: &str,
    compare_run_id: u64,
) -> SuccessfulCompareResult {
    let owner = owner(job_id, job_name, target_index, revision, compare_run_id);
    let plan_header = PlanHeader {
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
    };
    let plan_digest = syncdash::model::plan::Plan::digest_parts(&plan_header, &[]);
    let snapshot = |root: &str| TableArtifact {
        header: TableHeader {
            schema: TABLE_SCHEMA,
            kind: TableKind::Snapshot,
            root: root.into(),
            host: "host".into(),
            os: "test".into(),
            scanned_at_ms: 0,
            duration_ms: 0,
            entry_count: 0,
            evidence: TableEvidence::None,
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
        plan_digest,
        PlanDto {
            header: plan_header,
            ops: Vec::new(),
            metas: Vec::new(),
            identical_count: 0,
            identical_bytes: 0,
            owner,
        },
        snapshot("/source"),
        snapshot("/target"),
        syncdash::pipeline::compare::CompareOptions::default(),
    )
}

fn publish(repository: &CompareResultRepository, version: SuccessfulCompareResult) {
    let scope = CompareScope::from_identity(&version.owner.identity);
    let compare_run_id = version.owner.identity.compare_run_id;
    let verification = repository
        .begin_verification(scope, Some(compare_run_id))
        .unwrap();
    repository
        .publish_successful_version(&verification, version)
        .unwrap();
}

#[test]
fn exact_versions_survive_newer_publications_for_the_same_scope() {
    let repository = CompareResultRepository::in_memory();
    publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    publish(&repository, version("job-a", "A", 0, "revision-a", 2));

    let older = repository
        .get_exact(&identity("job-a", 0, "revision-a", 1))
        .unwrap()
        .unwrap();
    assert_eq!(older.identity().compare_run_id, 1);
    let latest = repository
        .latest_for("job-a", 0, "revision-a")
        .unwrap()
        .unwrap();
    assert_eq!(latest.identity().compare_run_id, 2);
}

#[test]
fn failed_or_cancelled_newer_compare_preserves_display_but_blocks_execution() {
    let repository = CompareResultRepository::in_memory();
    let retained_identity = identity("job-a", 0, "revision-a", 1);
    publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    assert!(repository.get_fresh_exact(&retained_identity).is_ok());

    let failed = repository
        .begin_verification(CompareScope::new("job-a", 0, "revision-a"), Some(2))
        .unwrap();
    assert!(repository.complete_verification_terminal(
        &failed,
        CompareVerificationTerminalOutcome::Failed {
            message: "network unavailable".into(),
        },
    ));

    assert!(repository.get_exact(&retained_identity).unwrap().is_some());
    assert_eq!(
        repository
            .latest_for("job-a", 0, "revision-a")
            .unwrap()
            .unwrap()
            .identity(),
        &retained_identity
    );
    let error = match repository.get_fresh_exact(&retained_identity) {
        Err(error) => error,
        Ok(_) => panic!("a failed newer Compare must leave the retained result non-executable"),
    };
    assert!(matches!(
        error,
        CompareResultRepositoryError::AwaitingSuccessfulCompare(_)
    ));
    let mut reserved = false;
    assert!(repository
        .with_fresh_execution_eligibility(&retained_identity, || {
            reserved = true;
            Ok(())
        })
        .is_err());
    assert!(!reserved);
    assert!(matches!(
        repository.execution_status_for_identity(&retained_identity),
        CompareScopeExecutionStatusDto::Failed { attempt, message, .. }
            if attempt.verification_epoch == 2
                && attempt.compare_run_id == Some(2)
                && message == "network unavailable"
    ));
}

#[test]
fn successful_republication_restores_only_the_new_exact_result() {
    let repository = CompareResultRepository::in_memory();
    let older_identity = identity("job-a", 0, "revision-a", 1);
    let newer_identity = identity("job-a", 0, "revision-a", 2);
    publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    let verification = repository
        .begin_verification(CompareScope::new("job-a", 0, "revision-a"), Some(2))
        .unwrap();
    repository
        .publish_successful_version(&verification, version("job-a", "A", 0, "revision-a", 2))
        .unwrap();

    assert!(repository.get_exact(&older_identity).unwrap().is_some());
    assert!(matches!(
        repository.get_fresh_exact(&older_identity),
        Err(CompareResultRepositoryError::ResultIsNotExecutionFresh { .. })
    ));
    assert_eq!(
        repository
            .get_fresh_exact(&newer_identity)
            .unwrap()
            .identity(),
        &newer_identity
    );
    assert_eq!(
        repository
            .with_fresh_execution_eligibility(&newer_identity, || Ok("reserved"))
            .unwrap(),
        "reserved"
    );
}

#[test]
fn awaiting_verification_cannot_publish_before_a_compare_run_launches() {
    let repository = CompareResultRepository::in_memory();
    let scope = CompareScope::new("job-a", 0, "revision-a");
    let verification = repository.begin_verification(scope.clone(), None).unwrap();
    let result_identity = identity("job-a", 0, "revision-a", 8);

    assert!(matches!(
        repository.publish_successful_version(
            &verification,
            version("job-a", "A", 0, "revision-a", 8),
        ),
        Err(CompareResultRepositoryError::VerificationHasNotLaunched(
            rejected_scope,
        )) if rejected_scope == scope
    ));
    assert!(repository.get_exact(&result_identity).unwrap().is_none());
    assert!(matches!(
        repository.execution_status(&scope),
        CompareScopeExecutionStatusDto::AwaitingCompare { attempt, .. }
            if attempt.verification_epoch == 1 && attempt.compare_run_id.is_none()
    ));
}

#[test]
fn launched_verification_rejects_a_result_from_another_run() {
    let repository = CompareResultRepository::in_memory();
    let scope = CompareScope::new("job-a", 0, "revision-a");
    let verification = repository
        .begin_verification(scope.clone(), Some(8))
        .unwrap();
    let wrong_identity = identity("job-a", 0, "revision-a", 9);

    assert!(matches!(
        repository
            .publish_successful_version(&verification, version("job-a", "A", 0, "revision-a", 9),),
        Err(CompareResultRepositoryError::VerificationRunMismatch {
            launched_run_id: 8,
            published_run_id: 9,
        })
    ));
    assert!(repository.get_exact(&wrong_identity).unwrap().is_none());
    assert!(matches!(
        repository.execution_status(&scope),
        CompareScopeExecutionStatusDto::Comparing { attempt, .. }
            if attempt.verification_epoch == 1 && attempt.compare_run_id == Some(8)
    ));
}

#[test]
fn superseded_success_is_rejected_without_retaining_evidence() {
    let repository = CompareResultRepository::in_memory();
    let scope = CompareScope::new("job-a", 0, "revision-a");
    let first = repository
        .begin_verification(scope.clone(), Some(1))
        .unwrap();
    let second = repository.begin_verification(scope, Some(2)).unwrap();
    let first_identity = identity("job-a", 0, "revision-a", 1);
    let second_identity = identity("job-a", 0, "revision-a", 2);

    assert!(matches!(
        repository.publish_successful_version(&first, version("job-a", "A", 0, "revision-a", 1),),
        Err(CompareResultRepositoryError::VerificationWasSuperseded {
            submitted_epoch: 1,
            active_epoch: 2,
        })
    ));
    assert!(repository.get_exact(&first_identity).unwrap().is_none());
    assert!(matches!(
        repository.execution_status_for_identity(&first_identity),
        CompareScopeExecutionStatusDto::Comparing { attempt, .. }
            if attempt.verification_epoch == 2 && attempt.compare_run_id == Some(2)
    ));
    assert!(matches!(
        repository.get_fresh_exact(&first_identity),
        Err(CompareResultRepositoryError::AwaitingSuccessfulCompare(_))
    ));
    repository
        .publish_successful_version(&second, version("job-a", "A", 0, "revision-a", 2))
        .unwrap();
    assert_eq!(
        repository
            .get_fresh_exact(&second_identity)
            .unwrap()
            .identity(),
        &second_identity
    );
}

#[test]
fn late_older_verification_cannot_publish_or_regress_current_pointers() {
    let repository = CompareResultRepository::in_memory();
    let scope = CompareScope::new("job-a", 0, "revision-a");
    let first = repository
        .begin_verification(scope.clone(), Some(1))
        .unwrap();
    let second = repository.begin_verification(scope, Some(2)).unwrap();
    let first_identity = identity("job-a", 0, "revision-a", 1);
    let second_identity = identity("job-a", 0, "revision-a", 2);

    repository
        .publish_successful_version(&second, version("job-a", "A", 0, "revision-a", 2))
        .unwrap();
    assert!(matches!(
        repository.publish_successful_version(&first, version("job-a", "A", 0, "revision-a", 1),),
        Err(CompareResultRepositoryError::VerificationWasSuperseded {
            submitted_epoch: 1,
            active_epoch: 2,
        })
    ));

    assert!(repository.get_exact(&first_identity).unwrap().is_none());
    assert_eq!(
        repository
            .latest_for("job-a", 0, "revision-a")
            .unwrap()
            .unwrap()
            .identity(),
        &second_identity
    );
    assert_eq!(
        repository
            .get_fresh_exact(&second_identity)
            .unwrap()
            .identity(),
        &second_identity
    );
}

#[test]
fn verification_epoch_exhaustion_stays_fail_closed() {
    let repository = CompareResultRepository::in_memory();
    let scope = CompareScope::new("job-a", 0, "revision-a");
    repository.store.lock().unwrap().execution_by_scope.insert(
        scope.clone(),
        CompareExecutionState::Fresh {
            verification_epoch: u64::MAX,
            identity: identity("job-a", 0, "revision-a", 1),
        },
    );

    assert!(matches!(
        repository.begin_verification(scope, Some(2)),
        Err(CompareResultRepositoryError::VerificationEpochExhausted(_))
    ));
    assert!(matches!(
        repository.get_fresh_exact(&identity("job-a", 0, "revision-a", 1)),
        Err(CompareResultRepositoryError::AwaitingSuccessfulCompare(_))
    ));
}

#[test]
fn final_reservation_and_new_verification_have_one_lock_order() {
    let repository = Arc::new(CompareResultRepository::in_memory());
    let result_identity = identity("job-a", 0, "revision-a", 1);
    publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    let (reservation_entered, entered) = std::sync::mpsc::channel();
    let (release_reservation, release) = std::sync::mpsc::channel();
    let reservation_repository = repository.clone();
    let reserved_identity = result_identity.clone();
    let reservation = std::thread::spawn(move || {
        reservation_repository.with_fresh_execution_eligibility(&reserved_identity, || {
            reservation_entered.send(()).unwrap();
            release.recv().unwrap();
            Ok(())
        })
    });
    entered.recv().unwrap();

    let verification_repository = repository.clone();
    let (verification_began, began) = std::sync::mpsc::channel();
    let verification = std::thread::spawn(move || {
        let ticket = verification_repository
            .begin_verification(CompareScope::new("job-a", 0, "revision-a"), Some(2))
            .unwrap();
        verification_began.send(ticket).unwrap();
    });
    assert!(began
        .recv_timeout(std::time::Duration::from_millis(25))
        .is_err());

    release_reservation.send(()).unwrap();
    reservation.join().unwrap().unwrap();
    began.recv().unwrap();
    verification.join().unwrap();
    assert!(matches!(
        repository.get_fresh_exact(&result_identity),
        Err(CompareResultRepositoryError::AwaitingSuccessfulCompare(_))
    ));
}

#[test]
fn bounded_hot_cache_never_changes_retention_or_latest_pointers() {
    let repository = CompareResultRepository::in_memory();
    publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    publish(&repository, version("job-b", "B", 0, "revision-b", 2));
    publish(&repository, version("job-c", "C", 0, "revision-c", 3));
    publish(&repository, version("job-d", "D", 0, "revision-d", 4));
    publish(&repository, version("job-e", "E", 0, "revision-e", 5));

    assert!(repository
        .latest_for("job-b", 0, "revision-b")
        .unwrap()
        .is_some());
    assert!(repository
        .latest_for("job-a", 0, "revision-a")
        .unwrap()
        .is_some());
}

#[test]
fn explicit_forget_removes_only_the_exact_result_and_its_latest_pointer() {
    let repository = CompareResultRepository::in_memory();
    publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    publish(&repository, version("job-b", "B", 0, "revision-b", 2));
    let forgotten = identity("job-b", 0, "revision-b", 2);
    assert!(matches!(
        repository.forget(&forgotten).unwrap(),
        CompareResultForgetOutcome::Forgotten {
            cleanup_warning: None
        }
    ));
    assert!(matches!(
        repository.forget(&forgotten).unwrap(),
        CompareResultForgetOutcome::AlreadyForgotten
    ));
    assert!(repository.get_exact(&forgotten).unwrap().is_none());
    assert!(repository
        .get_exact(&identity("job-a", 0, "revision-a", 1))
        .unwrap()
        .is_some());
    assert!(matches!(
        repository
            .restore_workspace("job-b", 0, "revision-b")
            .unwrap(),
        CompareWorkspaceLookupDto::Missing { .. }
    ));
}

#[test]
fn result_id_with_different_identity_fields_fails_closed() {
    let repository = CompareResultRepository::in_memory();
    let retained = identity("job-a", 0, "revision-a", 1);
    publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    let mut mismatched = retained.clone();
    mismatched.config_revision = "revision-b".to_string();

    assert!(matches!(
        repository.get_exact(&mismatched),
        Err(CompareResultRepositoryError::IdentityMismatch { result_id })
            if result_id == retained.result_id
    ));
    assert!(matches!(
        repository.forget(&mismatched),
        Err(CompareResultRepositoryError::IdentityMismatch { result_id })
            if result_id == retained.result_id
    ));
    assert!(repository.get_exact(&retained).unwrap().is_some());
}

#[test]
fn mutation_expiry_is_scoped_by_stable_job_identity_and_revision_without_deleting_evidence() {
    let repository = CompareResultRepository::in_memory();
    publish(&repository, version("job-a", "A", 0, "revision-old", 1));
    publish(&repository, version("job-a", "A", 0, "revision-current", 2));
    publish(&repository, version("job-b", "A", 0, "revision-old", 3));

    repository.expire_revision(
        "job-a",
        "revision-old",
        CompareExecutionExpiryReasonDto::JobChanged,
    );
    assert!(repository
        .get_exact(&identity("job-a", 0, "revision-old", 1))
        .unwrap()
        .is_some());
    assert!(repository
        .get_exact(&identity("job-a", 0, "revision-current", 2))
        .unwrap()
        .is_some());
    assert!(repository
        .get_exact(&identity("job-b", 0, "revision-old", 3))
        .unwrap()
        .is_some());

    assert!(matches!(
        repository.execution_status_for_identity(&identity("job-a", 0, "revision-old", 1)),
        CompareScopeExecutionStatusDto::Expired {
            reason: CompareExecutionExpiryReasonDto::JobChanged,
            ..
        }
    ));

    repository.expire_revision(
        "job-a",
        "revision-current",
        CompareExecutionExpiryReasonDto::WriteStarted,
    );
    assert!(matches!(
        repository.execution_status_for_identity(&identity("job-a", 0, "revision-current", 2)),
        CompareScopeExecutionStatusDto::Expired {
            reason: CompareExecutionExpiryReasonDto::WriteStarted,
            ..
        }
    ));

    repository.expire_job("job-a");
    assert!(repository
        .get_exact(&identity("job-a", 0, "revision-current", 2))
        .unwrap()
        .is_some());
    assert!(matches!(
        repository.execution_status_for_identity(&identity("job-a", 0, "revision-current", 2)),
        CompareScopeExecutionStatusDto::Expired {
            reason: CompareExecutionExpiryReasonDto::JobDeleted,
            ..
        }
    ));
}

#[test]
fn workspace_restore_reads_plan_and_execution_status_under_one_scope_epoch() {
    let repository = CompareResultRepository::in_memory();
    let scope = CompareScope::new("job-a", 0, "revision-a");
    let verification = repository
        .begin_verification(scope.clone(), Some(41))
        .unwrap();
    repository
        .publish_successful_version(&verification, version("job-a", "A", 0, "revision-a", 41))
        .unwrap();

    let workspace = repository
        .restore_workspace("job-a", 0, "revision-a")
        .unwrap();
    let CompareWorkspaceLookupDto::Found { workspace } = workspace else {
        panic!("the published Compare result must restore");
    };
    assert_eq!(workspace.plan.owner.identity.compare_run_id, 41);
    assert!(matches!(
        workspace.execution_status,
        CompareScopeExecutionStatusDto::Fresh { attempt, owner, .. }
            if attempt.verification_epoch == 1
                && attempt.compare_run_id == Some(41)
                && owner.identity == workspace.plan.owner.identity
    ));
}

#[test]
fn stale_terminal_completion_cannot_overwrite_a_newer_attempt_status() {
    let repository = CompareResultRepository::in_memory();
    let scope = CompareScope::new("job-a", 0, "revision-a");
    let older = repository
        .begin_verification(scope.clone(), Some(10))
        .unwrap();
    let current = repository
        .begin_verification(scope.clone(), Some(11))
        .unwrap();

    assert!(!repository.complete_verification_terminal(
        &older,
        CompareVerificationTerminalOutcome::Failed {
            message: "late failure".into(),
        },
    ));
    assert!(repository
        .complete_verification_terminal(&current, CompareVerificationTerminalOutcome::Cancelled,));
    assert!(matches!(
        repository.execution_status(&scope),
        CompareScopeExecutionStatusDto::Cancelled { attempt, .. }
            if attempt.verification_epoch == 2 && attempt.compare_run_id == Some(11)
    ));
}

#[test]
fn typed_prelaunch_terminal_states_do_not_invent_a_compare_run_identity() {
    let repository = CompareResultRepository::in_memory();
    let scope = CompareScope::new("job-a", 0, "revision-a");
    let cancelled = repository.begin_verification(scope.clone(), None).unwrap();

    assert!(
        repository.complete_verification_terminal(
            &cancelled,
            CompareVerificationTerminalOutcome::Cancelled,
        )
    );
    assert!(matches!(
        repository.execution_status(&scope),
        CompareScopeExecutionStatusDto::Cancelled { attempt, .. }
            if attempt.verification_epoch == 1 && attempt.compare_run_id.is_none()
    ));

    let failed = repository.begin_verification(scope.clone(), None).unwrap();
    assert!(repository.complete_verification_terminal(
        &failed,
        CompareVerificationTerminalOutcome::Failed {
            message: "cancelled".into(),
        },
    ));
    assert!(matches!(
        repository.execution_status(&scope),
        CompareScopeExecutionStatusDto::Failed { attempt, message, .. }
            if attempt.verification_epoch == 2
                && attempt.compare_run_id.is_none()
                && message == "cancelled"
    ));

    repository.expire_revision(
        "job-a",
        "revision-a",
        CompareExecutionExpiryReasonDto::JobChanged,
    );
    assert!(matches!(
        repository.execution_status(&scope),
        CompareScopeExecutionStatusDto::Expired { attempt, reason, .. }
            if attempt.verification_epoch == 2
                && attempt.compare_run_id.is_none()
                && reason == CompareExecutionExpiryReasonDto::JobChanged
    ));
}

#[test]
fn terminal_failure_cannot_be_reopened_by_a_late_success() {
    let repository = CompareResultRepository::in_memory();
    let scope = CompareScope::new("job-a", 0, "revision-a");
    let verification = repository.begin_verification(scope, Some(12)).unwrap();

    assert!(repository.complete_verification_terminal(
        &verification,
        CompareVerificationTerminalOutcome::Cancelled,
    ));
    assert!(!repository.complete_verification_terminal(
        &verification,
        CompareVerificationTerminalOutcome::Failed {
            message: "late failure".into(),
        },
    ));
    assert!(matches!(
        repository
            .publish_successful_version(&verification, version("job-a", "A", 0, "revision-a", 12),),
        Err(CompareResultRepositoryError::VerificationIsNotActive(_))
    ));
}

#[test]
fn reconciliation_after_configuration_change_keeps_the_plan_view_only() {
    let repository = CompareResultRepository::in_memory();
    let result = identity("job-a", 0, "revision-a", 7);
    publish(&repository, version("job-a", "A", 0, "revision-a", 7));

    let workspace = repository
        .reconcile_exact_workspace(&result, CompareWorkspaceJobState::ConfigurationChanged)
        .unwrap();
    let CompareWorkspaceLookupDto::Found { workspace } = workspace else {
        panic!("the retained Compare plan must remain viewable after expiry");
    };
    assert_eq!(workspace.plan.owner.identity, result);
    assert!(matches!(
        workspace.execution_status,
        CompareScopeExecutionStatusDto::Expired {
            reason: CompareExecutionExpiryReasonDto::JobChanged,
            ..
        }
    ));
    assert!(repository.get_fresh_exact(&result).is_err());
}

#[test]
fn missing_workspace_returns_its_terminal_execution_status_atomically() {
    let repository = CompareResultRepository::in_memory();
    let result = identity("job-a", 0, "revision-a", 7);
    repository
        .begin_verification(
            CompareScope::from_identity(&result),
            Some(result.compare_run_id),
        )
        .unwrap();

    let lookup = repository
        .reconcile_exact_workspace(&result, CompareWorkspaceJobState::Deleted)
        .unwrap();

    assert!(matches!(
        lookup,
        CompareWorkspaceLookupDto::Missing {
            execution_status: CompareScopeExecutionStatusDto::Expired {
                scope,
                attempt,
                reason: CompareExecutionExpiryReasonDto::JobDeleted,
            },
        } if scope == CompareScope::from_identity(&result).dto()
            && attempt.verification_epoch == 1
            && attempt.compare_run_id == Some(7)
    ));
}

#[test]
fn rename_rebinds_only_presentation_for_every_retained_version() {
    let repository = CompareResultRepository::in_memory();
    publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    publish(&repository, version("job-a", "A", 0, "revision-a", 2));

    repository.rebind_job_name("job-a", "Archive").unwrap();
    let older = repository
        .get_exact(&identity("job-a", 0, "revision-a", 1))
        .unwrap()
        .unwrap();
    assert_eq!(older.owner().job_name, "Archive");
    assert_eq!(older.plan().owner.job_name, "Archive");
    assert_eq!(older.identity().compare_run_id, 1);
}

#[test]
fn validation_requires_the_exact_retained_identity_and_plan_digest() {
    let repository = CompareResultRepository::in_memory();
    publish(&repository, version("job-a", "A", 1, "revision-a", 7));
    let owner = owner("job-a", "A", 1, "revision-a", 7);
    let retained = repository.get_exact(&owner.identity).unwrap();
    let plan_digest = retained.as_ref().unwrap().plan_digest().to_string();
    assert!(validate_retained_compare(
        retained.as_ref(),
        &owner,
        "job-a",
        "A",
        1,
        "revision-a",
        Some(&plan_digest),
    )
    .is_ok());

    let wrong_digest = validate_retained_compare(
        retained.as_ref(),
        &owner,
        "job-a",
        "A",
        1,
        "revision-a",
        Some("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
    )
    .unwrap_err();
    assert!(wrong_digest.contains("no longer matches"), "{wrong_digest}");

    let missing =
        validate_retained_compare(None, &owner, "job-a", "A", 1, "revision-a", None).unwrap_err();
    assert!(missing.contains("exact Compare result"), "{missing}");
}

#[test]
fn missing_presentation_state_fails_closed_instead_of_reusing_a_stale_plan_label() {
    let repository = CompareResultRepository::in_memory();
    publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    repository.store.lock().unwrap().job_names.remove("job-a");

    let error = match repository.get_exact(&identity("job-a", 0, "revision-a", 1)) {
        Err(error) => error,
        Ok(_) => panic!("missing presentation state must fail closed"),
    };
    assert_eq!(
        error,
        CompareResultRepositoryError::MissingJobDisplayName {
            job_id: "job-a".into()
        }
    );
}

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let token = crate::secure_random::random_hex::<8>("test directory").unwrap();
        let path = std::env::temp_dir().join(format!(
            "syncdash-compare-results-{label}-{}-{token}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn durable_result_restores_after_restart_but_execution_is_expired() {
    let directory = TestDirectory::new("restart");
    let result = identity("job-a", 0, "revision-a", 1);
    {
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    }

    let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
    assert!(repository.store.lock().unwrap().versions_by_id.is_empty());
    let restored = repository
        .restore_workspace("job-a", 0, "revision-a")
        .unwrap();
    assert!(matches!(
        restored,
        CompareWorkspaceLookupDto::Found { workspace }
            if workspace.plan.owner.identity == result
                && matches!(
                    workspace.execution_status,
                    CompareScopeExecutionStatusDto::Expired {
                        reason: CompareExecutionExpiryReasonDto::ApplicationRestarted,
                        ..
                    }
                )
    ));
    assert!(repository.get_fresh_exact(&result).is_err());
}

#[test]
fn stable_result_id_prevents_run_number_collision_after_restart() {
    let directory = TestDirectory::new("run-id-collision");
    let older = identity("job-a", 0, "revision-a", 1);
    {
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    }

    let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
    let mut newer_version = version("job-a", "A", 0, "revision-a", 1);
    newer_version.owner.identity.result_id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into();
    let newer = newer_version.owner.identity.clone();
    publish(&repository, newer_version);

    assert_eq!(older.compare_run_id, newer.compare_run_id);
    assert_ne!(older.result_id, newer.result_id);
    assert!(repository.get_exact(&older).unwrap().is_some());
    assert_eq!(
        repository
            .latest_for("job-a", 0, "revision-a")
            .unwrap()
            .unwrap()
            .identity(),
        &newer
    );
}

#[test]
fn hot_cache_eviction_never_changes_durable_retention() {
    let directory = TestDirectory::new("hot-cache");
    let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
    for run_id in 1..=6 {
        publish(
            &repository,
            version(
                &format!("job-{run_id}"),
                &format!("Job {run_id}"),
                0,
                &format!("revision-{run_id}"),
                run_id,
            ),
        );
    }
    {
        let store = repository.store.lock().unwrap();
        assert_eq!(store.retained_identities_by_id.len(), 6);
        assert_eq!(store.versions_by_id.len(), HOT_RESULT_CACHE_CAPACITY);
    }
    assert!(repository
        .get_exact(&identity("job-1", 0, "revision-1", 1))
        .unwrap()
        .is_some());
    assert_eq!(
        repository
            .store
            .lock()
            .unwrap()
            .retained_identities_by_id
            .len(),
        6
    );
}

#[test]
fn durable_forget_survives_restart_without_resurrecting_an_older_latest() {
    let directory = TestDirectory::new("forget");
    let older = identity("job-a", 0, "revision-a", 1);
    let latest = identity("job-a", 0, "revision-a", 2);
    {
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        publish(&repository, version("job-a", "A", 0, "revision-a", 2));
        assert!(matches!(
            repository.forget(&latest).unwrap(),
            CompareResultForgetOutcome::Forgotten {
                cleanup_warning: None
            }
        ));
    }

    let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
    assert!(repository.get_exact(&older).unwrap().is_some());
    assert!(repository.get_exact(&latest).unwrap().is_none());
    assert!(matches!(
        repository
            .restore_workspace("job-a", 0, "revision-a")
            .unwrap(),
        CompareWorkspaceLookupDto::Missing { .. }
    ));
}

#[test]
fn corrupt_artifact_blocks_repository_startup() {
    use std::io::Write as _;

    let directory = TestDirectory::new("corrupt");
    let result = identity("job-a", 0, "revision-a", 1);
    {
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    }
    let artifact = directory
        .0
        .join("results")
        .join(format!("{}.jsonl", result.result_id));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&artifact)
        .unwrap()
        .write_all(b"{}\n")
        .unwrap();

    assert!(matches!(
        CompareResultRepository::open_at(directory.0.clone()),
        Err(CompareResultRepositoryError::Storage(_))
    ));
}
