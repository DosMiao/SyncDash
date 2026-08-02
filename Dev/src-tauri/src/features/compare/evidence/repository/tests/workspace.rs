//! Reading a retained result back as a workspace, and refusing a stale or unproven one.
//!
//! Every path here fails closed: a terminal status cannot be reopened by a late success, a
//! reconciliation after a configuration change keeps the plan view-only, and a missing
//! presentation state refuses rather than reusing a stale label.

use crate::contracts::compare::{
    CompareExecutionExpiryReasonDto, CompareScopeExecutionStatusDto, CompareWorkspaceLookupDto,
};

use super::super::super::model::error::*;
use super::super::super::model::scope::*;
use super::super::super::model::verification::*;
use super::super::validation::validate_retained_compare;
use super::super::*;
use super::fixtures::*;

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
