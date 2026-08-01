import assert from 'node:assert/strict';
import test from 'node:test';

import {
  applyReviewKey,
  compareReviewKey,
  directAuthorization,
  EMPTY_APPROVAL_CHOICES,
  INITIAL_OPERATION_REVIEW,
  normalizeApprovalChoices,
  normalizedSelectedDecisions,
  operationReviewCanSubmit,
  operationReviewReducer,
  ownsOperationReviewTicket,
  reviewAllowsApproval,
  type ApprovalChoices,
  type OperationReviewTicket,
} from '../../typescript/ui/state/operation-review.ts';
import type { OperationReviewDto } from '../../typescript/core/types/generated/OperationReviewDto.ts';
import type { CompareOwner } from '../../typescript/core/types/generated/CompareOwner.ts';

const owner: CompareOwner = {
  compare_id: 71,
  job_id: 'job-id',
  job_name: 'before-rename',
  config_revision: 'revision-3',
  target_index: 1,
};

function review(overrides: Partial<OperationReviewDto> = {}): OperationReviewDto {
  return {
    status: 'confirmation_required',
    authorization: null,
    challenge_id: 'challenge-1',
    expires_at_ms: 10_000,
    blockers: [],
    warnings: ['deletion ratio is high'],
    capabilities: [],
    requires_health_ack: true,
    requires_capability_ack: true,
    can_remember_for_session: true,
    can_allow_unattended: true,
    ...overrides,
  };
}

test('reducer ignores every response that does not own the current key and generation', () => {
  const first: OperationReviewTicket = { key: compareReviewKey('job-id', 'revision-3', 1), generation: 1 };
  const second: OperationReviewTicket = { key: compareReviewKey('job-id', 'revision-4', 1), generation: 2 };
  let state = operationReviewReducer(INITIAL_OPERATION_REVIEW, { type: 'begin', ticket: first });
  state = operationReviewReducer(state, { type: 'begin', ticket: second });
  const stale = operationReviewReducer(state, { type: 'resolved', ticket: first, review: review() });
  assert.equal(stale, state);
  assert.equal(stale.phase, 'reviewing');

  state = operationReviewReducer(state, { type: 'resolved', ticket: second, review: review() });
  assert.equal(state.phase, 'ready');
  assert.equal(state.review?.challenge_id, 'challenge-1');
  const staleAuthorization = operationReviewReducer(state, {
    type: 'authorized',
    ticket: first,
    authorization: { authorization_token: 'stale-token', expires_at_ms: 20_000 },
  });
  assert.equal(staleAuthorization, state);
});

test('external async fencing requires ticket, generation, and current semantic identity', () => {
  const ticket = { key: compareReviewKey('job-id', 'revision-3', 1), generation: 9 };
  assert.equal(ownsOperationReviewTicket(ticket, ticket, ticket.key), true);
  assert.equal(ownsOperationReviewTicket({ ...ticket, generation: 10 }, ticket, ticket.key), false);
  assert.equal(ownsOperationReviewTicket(ticket, ticket, compareReviewKey('job-id', 'revision-4', 1)), false);
  assert.equal(ownsOperationReviewTicket(null, ticket, ticket.key), false);
});

test('approval is gated by each exact acknowledgement and blocked reviews can never submit', () => {
  const requested = review();
  const healthOnly: ApprovalChoices = { ...EMPTY_APPROVAL_CHOICES, acknowledgeHealth: true };
  const allRequired: ApprovalChoices = { ...healthOnly, acceptCapabilities: true };
  assert.equal(reviewAllowsApproval(requested, EMPTY_APPROVAL_CHOICES), false);
  assert.equal(reviewAllowsApproval(requested, healthOnly), false);
  assert.equal(reviewAllowsApproval(requested, allRequired), true);
  assert.equal(reviewAllowsApproval(review({ status: 'blocked', challenge_id: null }), allRequired), false);
  assert.equal(reviewAllowsApproval(review({ challenge_id: null }), allRequired), false);
  assert.equal(reviewAllowsApproval(review({ blockers: ['contradictory blocker'] }), allRequired), false);
  assert.equal(reviewAllowsApproval(review({
    authorization: { authorization_token: 'wrong-shape', expires_at_ms: 20_000 },
  }), allRequired), false);
  assert.equal(reviewAllowsApproval({ ...requested, status: 'unexpected' } as unknown as OperationReviewDto, allRequired), false);

  const ticket = { key: 'apply', generation: 1 };
  let state = operationReviewReducer(INITIAL_OPERATION_REVIEW, { type: 'begin', ticket });
  state = operationReviewReducer(state, { type: 'resolved', ticket, review: requested });
  assert.equal(operationReviewCanSubmit(state, allRequired), true);
  state = operationReviewReducer(state, { type: 'begin_approval', ticket });
  assert.equal(operationReviewCanSubmit(state, allRequired), false);
  state = operationReviewReducer(state, { type: 'approval_failed', ticket, error: 'expired' });
  assert.equal(state.phase, 'error');
  assert.equal(operationReviewCanSubmit(state, allRequired), false);
});

test('direct authorization fails closed when the structured review contradicts its status', () => {
  const authorized = review({
    status: 'direct_authorized',
    authorization: { authorization_token: 'token', expires_at_ms: 20_000 },
    challenge_id: null,
    requires_health_ack: false,
    requires_capability_ack: false,
  });
  assert.equal(directAuthorization(authorized)?.authorization_token, 'token');
  assert.equal(directAuthorization({ ...authorized, blockers: ['offline'] }), null);
  assert.equal(directAuthorization({ ...authorized, challenge_id: 'contradictory-challenge' }), null);
  assert.equal(directAuthorization({ ...authorized, requires_capability_ack: true }), null);
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

test('unattended approval is normalized off unless Remember is both supported and selected', () => {
  const requested = review();
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
});

test('keys fence semantic mutation and selected-set changes while preserving a pure rename', () => {
  const selected = [{ index: 8, flipped: true }, { index: 2, flipped: false }];
  const base = applyReviewKey(owner, 'job-id', 'revision-3', 1, selected);
  assert.equal(base, applyReviewKey({ ...owner, job_name: 'after-rename' }, 'job-id', 'revision-3', 1, selected));
  assert.equal(base, applyReviewKey(owner, 'job-id', 'revision-3', 1, [...selected].reverse()));
  assert.notEqual(base, applyReviewKey({ ...owner, compare_id: 72 }, 'job-id', 'revision-3', 1, selected));
  assert.notEqual(base, applyReviewKey(owner, 'replacement-id', 'revision-3', 1, selected));
  assert.notEqual(base, applyReviewKey(owner, 'job-id', 'revision-4', 1, selected));
  assert.notEqual(base, applyReviewKey(owner, 'job-id', 'revision-3', 0, selected));
  assert.notEqual(base, applyReviewKey(owner, 'job-id', 'revision-3', 1, [{ index: 8, flipped: true }, { index: 2, flipped: true }]));

  const compare = compareReviewKey('job-id', 'revision-3', 1);
  assert.equal(compare, compareReviewKey('job-id', 'revision-3', 1));
  assert.notEqual(compare, compareReviewKey('job-id', 'revision-4', 1));
});

test('selected-decision normalization is non-mutating and rejects ambiguous duplicate indices', () => {
  const selected = [{ index: 9, flipped: true }, { index: 1, flipped: false }];
  assert.deepEqual(normalizedSelectedDecisions(selected), [[1, false], [9, true]]);
  assert.deepEqual(selected, [{ index: 9, flipped: true }, { index: 1, flipped: false }]);
  assert.throws(
    () => normalizedSelectedDecisions([{ index: 4, flipped: false }, { index: 4, flipped: true }]),
    /duplicate selected row index 4/,
  );
});
