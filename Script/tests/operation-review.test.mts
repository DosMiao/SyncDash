import assert from 'node:assert/strict';
import test from 'node:test';

import {
  applyReviewKey,
  compareReviewKey,
  directAuthorization,
  EMPTY_APPROVAL_CHOICES,
  INITIAL_OPERATION_REVIEW,
  normalizeApprovalChoices,
  operationApprovalFromChoices,
  normalizedReviewedRowDecisions,
  operationReviewCanSubmit,
  operationReviewReducer,
  ownsOperationReviewRequest,
  reviewAllowsApproval,
  type ApprovalChoices,
  type ReviewRequestFence,
} from '#core/application/operations/operationReview.ts';
import type { CompareIdentity } from '#core/types/generated/CompareIdentity.ts';
import type { OperationReviewDto } from '#core/types/generated/OperationReviewDto.ts';
import { buildReviewedRowDecisions } from '#core/domain/compare/plan.ts';

const compareIdentity: CompareIdentity = {
  result_id: '33333333333333333333333333333333',
  compare_run_id: 71,
  job_id: 'job-id',
  config_revision: 'revision-3',
  target_index: 1,
};

type ApplyConfirmationReview = Extract<OperationReviewDto, {
  status: 'interactive_apply_confirmation_required';
}>;

function applyConfirmation(
  overrides: Partial<ApplyConfirmationReview> = {},
): ApplyConfirmationReview {
  return {
    status: 'interactive_apply_confirmation_required',
    challenge_id: 'challenge-1',
    expires_at_ms: 10_000,
    warnings: ['deletion ratio is high'],
    capabilities: [],
    ...overrides,
  };
}


test('reducer accepts only explicit transitions owned by the current request fence', () => {
  const first: ReviewRequestFence = {
    key: compareReviewKey('job-id', 'revision-3', 1),
    requestId: 1,
  };
  const second: ReviewRequestFence = {
    key: compareReviewKey('job-id', 'revision-4', 1),
    requestId: 2,
  };
  let state = operationReviewReducer(INITIAL_OPERATION_REVIEW, { type: 'begin', request: first });
  state = operationReviewReducer(state, { type: 'begin', request: second });
  const stale = operationReviewReducer(state, {
    type: 'resolved',
    request: first,
    review: applyConfirmation(),
  });
  assert.equal(stale, state);
  assert.equal(stale.phase, 'reviewing');

  state = operationReviewReducer(state, {
    type: 'resolved',
    request: second,
    review: applyConfirmation(),
  });
  assert.equal(state.phase, 'ready');
  assert.ok(state.review !== null && 'challenge_id' in state.review);
  assert.equal(state.review.challenge_id, 'challenge-1');
  const invalidTransition = operationReviewReducer(state, {
    type: 'authorized',
    request: second,
    authorization: { authorization_token: 'premature', expires_at_ms: 20_000 },
  });
  assert.equal(invalidTransition, state);
  const staleAuthorization = operationReviewReducer(state, {
    type: 'authorized',
    request: first,
    authorization: { authorization_token: 'stale-token', expires_at_ms: 20_000 },
  });
  assert.equal(staleAuthorization, state);
});

test('external async fencing requires request id and current semantic identity', () => {
  const request = { key: compareReviewKey('job-id', 'revision-3', 1), requestId: 9 };
  assert.equal(ownsOperationReviewRequest(request, request, request.key), true);
  assert.equal(ownsOperationReviewRequest({ ...request, requestId: 10 }, request, request.key), false);
  assert.equal(
    ownsOperationReviewRequest(request, request, compareReviewKey('job-id', 'revision-4', 1)),
    false,
  );
  assert.equal(ownsOperationReviewRequest(null, request, request.key), false);
});

test('a review that reached confirmation is approvable, and blocked reviews never submit', () => {
  const requested = applyConfirmation();
  const allRequired: ApprovalChoices = EMPTY_APPROVAL_CHOICES;
  // Nothing in the panel is a condition of approval: no choice at all still submits.
  assert.equal(reviewAllowsApproval(requested, EMPTY_APPROVAL_CHOICES), true);
  assert.equal(reviewAllowsApproval(requested, allRequired), true);
  // A capability shortfall is reported, never a reason to withhold approval.
  assert.equal(reviewAllowsApproval({
    ...requested,
    capabilities: [{
      feature: 'root lock',
      side: 'target',
      severity: 'unavailable',
      requested: 'exclusion',
      actual: 'none',
      effect: 'stated',
    }],
  }, allRequired), true);
  assert.equal(reviewAllowsApproval({
    status: 'blocked',
    blockers: ['offline'],
    warnings: [],
    capabilities: [],
  }, allRequired), false);
  assert.equal(
    reviewAllowsApproval({ ...requested, status: 'unexpected' } as unknown as OperationReviewDto, allRequired),
    false,
  );

  const request = { key: 'apply', requestId: 1 };
  let state = operationReviewReducer(INITIAL_OPERATION_REVIEW, { type: 'begin', request });
  state = operationReviewReducer(state, { type: 'resolved', request, review: requested });
  assert.equal(operationReviewCanSubmit(state, allRequired, 0), true);
  state = operationReviewReducer(state, { type: 'begin_approval', request });
  assert.equal(operationReviewCanSubmit(state, allRequired, 0), false);
  state = operationReviewReducer(state, { type: 'approval_failed', request, error: 'expired' });
  assert.equal(state.phase, 'approval_failed');
  assert.equal(operationReviewCanSubmit(state, allRequired, 0), false);
});

test('challenge and authorization deadlines are executable state, not display text', () => {
  const request = { key: 'apply', requestId: 4 };
  const choices: ApprovalChoices = EMPTY_APPROVAL_CHOICES;
  let state = operationReviewReducer(INITIAL_OPERATION_REVIEW, { type: 'begin', request });
  state = operationReviewReducer(state, {
    type: 'resolved', request, review: applyConfirmation({ expires_at_ms: 10_000 }),
  });
  assert.equal(operationReviewCanSubmit(state, choices, 8_999), true);
  assert.equal(operationReviewCanSubmit(state, choices, 9_000), false);
  state = operationReviewReducer(state, {
    type: 'expired', request, error: 'review again',
  });
  assert.equal(state.phase, 'expired');
  assert.equal(operationReviewCanSubmit(state, choices, 0), false);

  const stale = operationReviewReducer(state, {
    type: 'expired', request: { ...request, requestId: 5 }, error: 'stale',
  });
  assert.equal(stale, state);

  let authorized = operationReviewReducer(INITIAL_OPERATION_REVIEW, { type: 'begin', request });
  authorized = operationReviewReducer(authorized, {
    type: 'resolved', request, review: applyConfirmation(),
  });
  authorized = operationReviewReducer(authorized, { type: 'begin_approval', request });
  authorized = operationReviewReducer(authorized, {
    type: 'authorized',
    request,
    authorization: { authorization_token: 'token', expires_at_ms: 20_000 },
  });
  assert.equal(operationReviewCanSubmit(authorized, choices, 18_999), true);
  assert.equal(operationReviewCanSubmit(authorized, choices, 19_000), false);
});

test('direct authorization accepts only its tagged variant and rejects capability blockers', () => {
  const authorized: OperationReviewDto = {
    status: 'direct_authorized',
    authorization: { authorization_token: 'token', expires_at_ms: 20_000 },
    capabilities: [],
  };
  assert.equal(directAuthorization(authorized)?.authorization_token, 'token');
  // A direct authorization stands on its own; the capability list rides along as information.
  assert.equal(directAuthorization({
    ...authorized,
    capabilities: [{
      feature: 'write',
      side: 'target',
      severity: 'unavailable',
      requested: 'yes',
      actual: 'no',
      effect: 'stated',
    }],
  })?.authorization_token, 'token');
});

test('an approval names the reviewed operation and carries no decisions of its own', () => {
  const requested = applyConfirmation();
  assert.deepEqual(normalizeApprovalChoices(requested, EMPTY_APPROVAL_CHOICES), {});
  assert.deepEqual(operationApprovalFromChoices(requested, EMPTY_APPROVAL_CHOICES), {
    operation: 'interactive_apply',
  });
});

test('keys fence result identity, job revision, target, and reviewed row decisions', () => {
  const reviewedRowDecisions = [
    { index: 8, direction_reversed: true },
    { index: 2, direction_reversed: false },
  ];
  const base = applyReviewKey(compareIdentity, 'job-id', 'revision-3', 1, 8, reviewedRowDecisions);
  assert.equal(base, applyReviewKey(compareIdentity, 'job-id', 'revision-3', 1, 8, [...reviewedRowDecisions].reverse()));
  assert.notEqual(base, applyReviewKey({ ...compareIdentity, result_id: '55555555555555555555555555555555' }, 'job-id', 'revision-3', 1, 8, reviewedRowDecisions));
  assert.notEqual(base, applyReviewKey({ ...compareIdentity, compare_run_id: 72 }, 'job-id', 'revision-3', 1, 8, reviewedRowDecisions));
  assert.notEqual(base, applyReviewKey({ ...compareIdentity, job_id: 'replacement-id' }, 'job-id', 'revision-3', 1, 8, reviewedRowDecisions));
  assert.notEqual(base, applyReviewKey({ ...compareIdentity, config_revision: 'revision-4' }, 'job-id', 'revision-3', 1, 8, reviewedRowDecisions));
  assert.notEqual(base, applyReviewKey({ ...compareIdentity, target_index: 0 }, 'job-id', 'revision-3', 1, 8, reviewedRowDecisions));
  assert.notEqual(base, applyReviewKey(compareIdentity, 'replacement-id', 'revision-3', 1, 8, reviewedRowDecisions));
  assert.notEqual(base, applyReviewKey(compareIdentity, 'job-id', 'revision-4', 1, 8, reviewedRowDecisions));
  assert.notEqual(base, applyReviewKey(compareIdentity, 'job-id', 'revision-3', 0, 8, reviewedRowDecisions));
  assert.notEqual(base, applyReviewKey(compareIdentity, 'job-id', 'revision-3', 1, 9, reviewedRowDecisions));
  assert.notEqual(base, applyReviewKey(compareIdentity, 'job-id', 'revision-3', 1, 8, [
    { index: 8, direction_reversed: true },
    { index: 2, direction_reversed: true },
  ]));

  const compare = compareReviewKey('job-id', 'revision-3', 1);
  assert.equal(compare, compareReviewKey('job-id', 'revision-3', 1));
  assert.notEqual(compare, compareReviewKey('job-id', 'revision-4', 1));
});

test('reviewed-row-decision normalization is non-mutating and rejects duplicate indices', () => {
  const reviewedRowDecisions = [
    { index: 9, direction_reversed: true },
    { index: 1, direction_reversed: false },
  ];
  assert.deepEqual(normalizedReviewedRowDecisions(reviewedRowDecisions), [[1, false], [9, true]]);
  assert.deepEqual(reviewedRowDecisions, [
    { index: 9, direction_reversed: true },
    { index: 1, direction_reversed: false },
  ]);
  assert.throws(
    () => normalizedReviewedRowDecisions([
      { index: 4, direction_reversed: false },
      { index: 4, direction_reversed: true },
    ]),
    /duplicate reviewed row index 4/,
  );
});

test('reviewed row decisions carry only plan identity and effective direction', () => {
  assert.deepEqual(buildReviewedRowDecisions([3, 1], [false, true, false, true]), [
    { index: 3, direction_reversed: true },
    { index: 1, direction_reversed: true },
  ]);
});
