//! Forgetting a result, and the identity fences around what a forget may reach.
//!
//! Expiry and forget are different operations. Expiry marks a result unexecutable and leaves the
//! plan viewable; only an explicit forget removes evidence, and only for the exact result named.

use crate::contracts::compare::{
    CompareExecutionExpiryReasonDto, CompareScopeExecutionStatusDto, CompareWorkspaceLookupDto,
};

use super::super::super::model::error::*;
use super::super::*;
use super::fixtures::*;

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
