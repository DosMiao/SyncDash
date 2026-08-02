//! Which Compare results may be published, and what a rejected one leaves behind.
//!
//! The rule these protect is that a newer Compare never silently discards the evidence of an
//! older one the user has not dismissed: a failed or cancelled run blocks execution while the
//! earlier plan stays viewable, and a superseded success is rejected without retaining anything.
use std::sync::Arc;

use crate::contracts::compare::CompareScopeExecutionStatusDto;

use super::super::super::model::error::*;
use super::super::super::model::execution::*;
use super::super::super::model::scope::*;
use super::super::super::model::verification::*;
use super::super::*;
use super::fixtures::*;

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
