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
  normalizedSelectedDecisions,
  operationReviewCanSubmit,
  operationReviewReducer,
  ownsOperationReviewRequest,
  reviewAllowsApproval,
  type ApprovalChoices,
  type ReviewRequestFence,
} from '../../typescript/ui/state/operationReview.ts';
import type { CompareIdentity } from '../../typescript/core/types/generated/CompareIdentity.ts';
import type { OperationReviewDto } from '../../typescript/core/types/generated/OperationReviewDto.ts';

const compareIdentity: CompareIdentity = {
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
    requires_health_ack: true,
    requires_capability_ack: true,
    can_remember_for_session: true,
    can_allow_unattended: true,
    ...overrides,
  };
}

function compareConfirmation(): Extract<OperationReviewDto, {
  status: 'compare_confirmation_required';
}> {
  return {
    status: 'compare_confirmation_required',
    challenge_id: 'compare-challenge',
    expires_at_ms: 10_000,
    capabilities: [],
    can_remember_for_session: true,
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
  assert.equal(state.review?.challenge_id, 'challenge-1');
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

test('approval is gated by each exact acknowledgement and blocked reviews never submit', () => {
  const requested = applyConfirmation();
  const healthOnly: ApprovalChoices = { ...EMPTY_APPROVAL_CHOICES, acknowledgeHealth: true };
  const allRequired: ApprovalChoices = { ...healthOnly, acceptCapabilities: true };
  assert.equal(reviewAllowsApproval(requested, EMPTY_APPROVAL_CHOICES), false);
  assert.equal(reviewAllowsApproval(requested, healthOnly), false);
  assert.equal(reviewAllowsApproval(requested, allRequired), true);
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
  assert.equal(operationReviewCanSubmit(state, allRequired), true);
  state = operationReviewReducer(state, { type: 'begin_approval', request });
  assert.equal(operationReviewCanSubmit(state, allRequired), false);
  state = operationReviewReducer(state, { type: 'approval_failed', request, error: 'expired' });
  assert.equal(state.phase, 'approval_failed');
  assert.equal(operationReviewCanSubmit(state, allRequired), false);
});

test('direct authorization accepts only its tagged variant and rejects capability blockers', () => {
  const authorized: OperationReviewDto = {
    status: 'direct_authorized',
    authorization: { authorization_token: 'token', expires_at_ms: 20_000 },
    capabilities: [],
  };
  assert.equal(directAuthorization(authorized)?.authorization_token, 'token');
  assert.equal(directAuthorization({
    ...authorized,
    capabilities: [{
      feature: 'write',
      side: 'target',
      severity: 'block',
      requested: 'yes',
      actual: 'no',
      effect: 'cannot run',
    }],
  }), null);
});

test('approval payload variants cannot contain fields from another operation', () => {
  const requested = applyConfirmation();
  const askUnattended: ApprovalChoices = {
    acknowledgeHealth: true,
    acceptCapabilities: true,
    rememberForSession: false,
    allowUnattended: true,
  };
  assert.deepEqual(normalizeApprovalChoices(requested, askUnattended), {
    acknowledgeHealth: true,
    acceptCapabilities: true,
    rememberForSession: false,
    allowUnattended: false,
  });
  assert.equal(reviewAllowsApproval(requested, askUnattended), false);
  const remembered = { ...askUnattended, rememberForSession: true };
  assert.equal(normalizeApprovalChoices(requested, remembered).allowUnattended, true);
  assert.deepEqual(operationApprovalFromChoices(requested, remembered), {
    operation: 'interactive_apply',
    acknowledge_health: true,
    accept_capabilities: true,
    session_grant: 'allow_auto_apply',
  });
  assert.deepEqual(operationApprovalFromChoices(compareConfirmation(), remembered), {
    operation: 'compare',
    accept_capabilities: true,
    remember_for_session: true,
  });
});

test('keys fence result identity, job revision, target, and selected decisions', () => {
  const selected = [{ index: 8, flipped: true }, { index: 2, flipped: false }];
  const base = applyReviewKey(compareIdentity, 'job-id', 'revision-3', 1, selected);
  assert.equal(base, applyReviewKey(compareIdentity, 'job-id', 'revision-3', 1, [...selected].reverse()));
  assert.notEqual(base, applyReviewKey({ ...compareIdentity, compare_run_id: 72 }, 'job-id', 'revision-3', 1, selected));
  assert.notEqual(base, applyReviewKey({ ...compareIdentity, job_id: 'replacement-id' }, 'job-id', 'revision-3', 1, selected));
  assert.notEqual(base, applyReviewKey({ ...compareIdentity, config_revision: 'revision-4' }, 'job-id', 'revision-3', 1, selected));
  assert.notEqual(base, applyReviewKey({ ...compareIdentity, target_index: 0 }, 'job-id', 'revision-3', 1, selected));
  assert.notEqual(base, applyReviewKey(compareIdentity, 'replacement-id', 'revision-3', 1, selected));
  assert.notEqual(base, applyReviewKey(compareIdentity, 'job-id', 'revision-4', 1, selected));
  assert.notEqual(base, applyReviewKey(compareIdentity, 'job-id', 'revision-3', 0, selected));
  assert.notEqual(base, applyReviewKey(compareIdentity, 'job-id', 'revision-3', 1, [
    { index: 8, flipped: true },
    { index: 2, flipped: true },
  ]));

  const compare = compareReviewKey('job-id', 'revision-3', 1);
  assert.equal(compare, compareReviewKey('job-id', 'revision-3', 1));
  assert.notEqual(compare, compareReviewKey('job-id', 'revision-4', 1));
});

test('selected-decision normalization is non-mutating and rejects duplicate indices', () => {
  const selected = [{ index: 9, flipped: true }, { index: 1, flipped: false }];
  assert.deepEqual(normalizedSelectedDecisions(selected), [[1, false], [9, true]]);
  assert.deepEqual(selected, [{ index: 9, flipped: true }, { index: 1, flipped: false }]);
  assert.throws(
    () => normalizedSelectedDecisions([{ index: 4, flipped: false }, { index: 4, flipped: true }]),
    /duplicate selected row index 4/,
  );
});
