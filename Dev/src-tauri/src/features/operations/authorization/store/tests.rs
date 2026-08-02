use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use crate::contracts::compare::{CompareIdentity, ReviewedRowDecisionDto};
use crate::features::autoscan::authority::AutoApplyTicket;
use crate::features::compare::evidence::model::scope::CompareScope;

use super::super::{apply::*, challenge::*, compare::*, digest::*, target::*};
use super::state::*;
use super::*;

fn compare_authorization() -> CompareAuthorization {
    CompareAuthorization::new(
        JobTargetRevision::new("job-a".into(), "revision-a".into(), 1).unwrap(),
        "caps-a".into(),
        CompareOrigin::Interactive,
    )
    .unwrap()
}

fn identity() -> CompareIdentity {
    CompareIdentity {
        result_id: "33333333333333333333333333333333".into(),
        compare_run_id: 9,
        job_id: "job-a".into(),
        config_revision: "revision-a".into(),
        target_index: 1,
    }
}

fn apply_review() -> ApplyReview {
    ApplyReview::new(
        identity(),
        "plan-a".into(),
        vec![ReviewedRowDecisionDto {
            index: 3,
            direction_reversed: true,
        }],
        "health-a".into(),
        "caps-a".into(),
    )
    .unwrap()
}

#[test]
fn reviewed_decisions_and_digest_are_created_atomically() {
    assert!(ExactReviewedDecisions::new(Vec::new()).is_err());
    assert!(ExactReviewedDecisions::new(vec![
        ReviewedRowDecisionDto {
            index: 2,
            direction_reversed: false,
        },
        ReviewedRowDecisionDto {
            index: 2,
            direction_reversed: true,
        },
    ])
    .is_err());
    let a = reviewed_row_decisions_digest(&[
        ReviewedRowDecisionDto {
            index: 2,
            direction_reversed: false,
        },
        ReviewedRowDecisionDto {
            index: 1,
            direction_reversed: true,
        },
    ])
    .unwrap();
    let b = reviewed_row_decisions_digest(&[
        ReviewedRowDecisionDto {
            index: 1,
            direction_reversed: true,
        },
        ReviewedRowDecisionDto {
            index: 2,
            direction_reversed: false,
        },
    ])
    .unwrap();
    assert_eq!(a, b);
}

#[test]
fn apply_review_verifies_the_stable_compare_identity() {
    let expected = apply_review();
    let current = ApplyReview::new(
        identity(),
        "plan-a".into(),
        expected.reviewed_row_decisions().to_vec(),
        "health-a".into(),
        "caps-a".into(),
    )
    .unwrap();
    assert!(expected.verify_current(&current).is_ok());
}

fn approve_apply_grant(
    store: &OperationAuthorizationStore,
    review: ApplyReview,
    session_grant: ApplySessionGrantDecision,
) -> IssuedAuthorization {
    let challenge = store
        .create_review_challenge(ReviewChallenge::InteractiveApply {
            review,
            requires_health_ack: false,
            requires_capability_ack: true,
        })
        .unwrap();
    store
        .approve_review_challenge(
            &challenge.challenge_id,
            ReviewApproval::InteractiveApply {
                acknowledge_health: false,
                accept_capabilities: true,
                session_grant,
            },
        )
        .unwrap()
}

#[test]
fn wrong_consumer_burns_token_and_parallel_consume_has_one_winner() {
    let store = Arc::new(OperationAuthorizationStore::default());
    let issued = store
        .issue_compare_authorization(compare_authorization())
        .unwrap();
    assert!(store
        .consume_apply_authorization(&issued.authorization_token)
        .is_err());
    assert!(store
        .consume_compare_authorization(&issued.authorization_token)
        .is_err());

    let issued = store
        .issue_compare_authorization(compare_authorization())
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let store = store.clone();
        let barrier = barrier.clone();
        let token = issued.authorization_token.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            store.consume_compare_authorization(&token).is_ok()
        }));
    }
    barrier.wait();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
}

#[test]
fn wrong_approval_variant_burns_challenge() {
    let store = OperationAuthorizationStore::default();
    let challenge = store
        .create_review_challenge(ReviewChallenge::Compare {
            authorization: compare_authorization(),
            requires_capability_ack: true,
        })
        .unwrap();
    let wrong = ReviewApproval::InteractiveApply {
        acknowledge_health: false,
        accept_capabilities: true,
        session_grant: ApplySessionGrantDecision::None,
    };
    assert!(store
        .approve_review_challenge(&challenge.challenge_id, wrong)
        .is_err());
    assert!(store
        .approve_review_challenge(
            &challenge.challenge_id,
            ReviewApproval::Compare {
                accept_capabilities: true,
                remember_for_session: false,
            },
        )
        .is_err());
}

#[test]
fn failed_token_creation_cannot_commit_a_session_grant() {
    let store = OperationAuthorizationStore::default();
    let challenge = store
        .create_review_challenge(ReviewChallenge::InteractiveApply {
            review: apply_review(),
            requires_health_ack: false,
            requires_capability_ack: true,
        })
        .unwrap();

    let result = store.approve_review_challenge_at_with_token(
        &challenge.challenge_id,
        ReviewApproval::InteractiveApply {
            acknowledge_health: false,
            accept_capabilities: true,
            session_grant: ApplySessionGrantDecision::AllowAutoApply,
        },
        Instant::now(),
        || Err("entropy source unavailable".into()),
    );

    assert!(result.is_err());
    let state = store.0.lock().unwrap();
    assert!(state.challenges.is_empty());
    assert!(state.authorizations.is_empty());
    assert!(state.grants.is_empty());
}

#[test]
fn expired_authorization_is_burned_before_it_is_reported() {
    let store = OperationAuthorizationStore::default();
    let issued = store
        .issue_compare_authorization(compare_authorization())
        .unwrap();
    store
        .0
        .lock()
        .unwrap()
        .authorizations
        .front_mut()
        .unwrap()
        .expires = Instant::now() - Duration::from_secs(1);

    assert!(store
        .consume_compare_authorization(&issued.authorization_token)
        .unwrap_err()
        .contains("expired"));
    assert!(store
        .consume_compare_authorization(&issued.authorization_token)
        .is_err());
}

#[test]
fn grants_cannot_cross_scope_revision_target_or_capability_digest() {
    let store = OperationAuthorizationStore::default();
    let compare = compare_authorization();
    let challenge = store
        .create_review_challenge(ReviewChallenge::Compare {
            authorization: compare.clone(),
            requires_capability_ack: true,
        })
        .unwrap();
    store
        .approve_review_challenge(
            &challenge.challenge_id,
            ReviewApproval::Compare {
                accept_capabilities: true,
                remember_for_session: true,
            },
        )
        .unwrap();
    assert!(store.has_compare_capability_grant(&compare));

    let revised = CompareAuthorization::new(
        JobTargetRevision::new("job-a".into(), "revision-b".into(), 1).unwrap(),
        "caps-a".into(),
        CompareOrigin::Interactive,
    )
    .unwrap();
    let retargeted = CompareAuthorization::new(
        JobTargetRevision::new("job-a".into(), "revision-a".into(), 0).unwrap(),
        "caps-a".into(),
        CompareOrigin::Interactive,
    )
    .unwrap();
    let recapped = CompareAuthorization::new(
        JobTargetRevision::new("job-a".into(), "revision-a".into(), 1).unwrap(),
        "caps-b".into(),
        CompareOrigin::Interactive,
    )
    .unwrap();
    assert!(!store.has_compare_capability_grant(&revised));
    assert!(!store.has_compare_capability_grant(&retargeted));
    assert!(!store.has_compare_capability_grant(&recapped));
    assert!(!store.has_interactive_apply_capability_grant(&apply_review()));
}

#[test]
fn auto_apply_requires_an_exact_grant_and_downgrade_revokes_issued_tokens() {
    let store = OperationAuthorizationStore::default();
    let review = apply_review();
    let ticket = AutoApplyTicket::for_test(4, 12, identity());
    assert!(store
        .issue_auto_apply_authorization(review.clone(), ticket.clone())
        .is_err());

    approve_apply_grant(
        &store,
        review.clone(),
        ApplySessionGrantDecision::AllowAutoApply,
    );
    let issued = store
        .issue_auto_apply_authorization(review.clone(), ticket)
        .unwrap();

    approve_apply_grant(
        &store,
        review,
        ApplySessionGrantDecision::RememberCapabilities,
    );
    assert!(store
        .consume_apply_authorization(&issued.authorization_token)
        .is_err());
}

#[test]
fn dirty_scope_revokes_apply_challenges_and_tokens_but_retains_session_consent() {
    let store = OperationAuthorizationStore::default();
    let review = apply_review();
    let pending = store
        .create_review_challenge(ReviewChallenge::InteractiveApply {
            review: review.clone(),
            requires_health_ack: false,
            requires_capability_ack: false,
        })
        .unwrap();
    let interactive = approve_apply_grant(
        &store,
        review.clone(),
        ApplySessionGrantDecision::AllowAutoApply,
    );
    let automatic = store
        .issue_auto_apply_authorization(
            review.clone(),
            AutoApplyTicket::for_test(4, 12, identity()),
        )
        .unwrap();

    store.revoke_apply_authority(&CompareScope::new("job-a", 1, "revision-a"));

    assert!(store
        .approve_review_challenge(
            &pending.challenge_id,
            ReviewApproval::InteractiveApply {
                acknowledge_health: false,
                accept_capabilities: false,
                session_grant: ApplySessionGrantDecision::None,
            },
        )
        .is_err());
    assert!(store
        .consume_apply_authorization(&interactive.authorization_token)
        .is_err());
    assert!(store
        .consume_apply_authorization(&automatic.authorization_token)
        .is_err());
    assert!(store.has_interactive_apply_capability_grant(&review));
    assert!(store.find_grant(|grant| grant.allows_apply(&review, true)));
}

#[test]
fn every_apply_fingerprint_and_health_message_set_is_exact() {
    let expected = apply_review();
    let mut changed_identity = identity();
    changed_identity.compare_run_id += 1;
    let changed = ApplyReview::new(
        changed_identity,
        "plan-a".into(),
        expected.reviewed_row_decisions().to_vec(),
        "health-a".into(),
        "caps-a".into(),
    )
    .unwrap();
    assert!(expected.verify_current(&changed).is_err());

    let first = syncdash::pipeline::guard::Verdict {
        blockers: vec!["b".into(), "a".into()],
        warnings: vec!["w".into()],
    };
    let reordered = syncdash::pipeline::guard::Verdict {
        blockers: vec!["a".into(), "b".into()],
        warnings: vec!["w".into()],
    };
    let acknowledged = syncdash::pipeline::guard::Verdict {
        blockers: Vec::new(),
        warnings: vec!["w".into()],
    };
    assert_eq!(
        health_review_digest(&first, &acknowledged),
        health_review_digest(&reordered, &acknowledged)
    );
    assert_ne!(
        health_review_digest(&first, &acknowledged),
        health_review_digest(&acknowledged, &first)
    );
}

#[test]
fn authorization_capacity_and_job_revocation_remain_exact() {
    let store = OperationAuthorizationStore::default();
    for index in 0..(AUTHORIZATION_CAPACITY + 5) {
        let job_id = if index % 2 == 0 { "job-a" } else { "job-b" };
        store
            .issue_compare_authorization(
                CompareAuthorization::new(
                    JobTargetRevision::new(job_id.into(), "revision-a".into(), 0).unwrap(),
                    format!("caps-{index}"),
                    CompareOrigin::Interactive,
                )
                .unwrap(),
            )
            .unwrap();
    }
    assert_eq!(
        store.0.lock().unwrap().authorizations.len(),
        AUTHORIZATION_CAPACITY
    );
    store.revoke_job_authority("job-a");
    let state = store.0.lock().unwrap();
    assert!(state
        .authorizations
        .iter()
        .all(|record| match &record.authorization {
            OperationAuthorization::Compare(authorization) => {
                authorization.target().job_id() == "job-b"
            }
            _ => false,
        }));
    assert!(state.authorizations.iter().any(|_| true));
}

#[test]
fn remembered_apply_capability_consent_is_reused_without_auto_apply_permission() {
    let store = OperationAuthorizationStore::default();
    let review = apply_review();
    let challenge = store
        .create_review_challenge(ReviewChallenge::InteractiveApply {
            review: review.clone(),
            requires_health_ack: false,
            requires_capability_ack: true,
        })
        .unwrap();
    store
        .approve_review_challenge(
            &challenge.challenge_id,
            ReviewApproval::InteractiveApply {
                acknowledge_health: false,
                accept_capabilities: true,
                session_grant: ApplySessionGrantDecision::RememberCapabilities,
            },
        )
        .unwrap();
    assert!(store.has_interactive_apply_capability_grant(&review));
    assert!(!store.find_grant(|grant| grant.allows_apply(&review, true)));
}
