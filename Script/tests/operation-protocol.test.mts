import assert from 'node:assert/strict';
import test from 'node:test';

import {
  applyAuthorizationArgs,
  approveOperationArgs,
  autoScanApplyAuthorizationArgs,
  compareAuthorizationArgs,
  reviewApplyArgs,
  reviewCompareArgs,
  startAutoScanArgs,
} from '#core/application/operations/operationProtocol.ts';
import type { CompareOwner } from '#core/types/generated/CompareOwner.ts';

const owner: CompareOwner = {
  identity: {
    result_id: '22222222222222222222222222222222',
    compare_run_id: 41,
    job_id: 'job-stable-id',
    config_revision: 'revision-7',
    target_index: 2,
  },
  job_name: 'photos',
};
const reviewedRowDecisions = [
  { index: 3, direction_reversed: false },
  { index: 9, direction_reversed: true },
];

test('Tauri argument mapping uses camelCase only at the boundary', () => {
  assert.deepEqual(startAutoScanArgs('job-stable-id', 'revision-7', 2), {
    expectedJobId: 'job-stable-id', expectedRevision: 'revision-7', targetIndex: 2,
  });
  assert.deepEqual(reviewCompareArgs('job-stable-id', 2), {
    expectedJobId: 'job-stable-id', targetIndex: 2,
  });
  assert.deepEqual(reviewCompareArgs('job-stable-id'), { expectedJobId: 'job-stable-id' });
  assert.deepEqual(reviewCompareArgs('job-stable-id', 2, { generation: 8, ticket_id: 13 }), {
    expectedJobId: 'job-stable-id',
    targetIndex: 2,
    autoScanRequest: { generation: 8, ticket_id: 13 },
  });
  assert.deepEqual(approveOperationArgs('challenge', {
    operation: 'interactive_apply',
    acknowledge_health: true,
    accept_capabilities: false,
    session_grant: 'remember_capabilities',
  }), {
    challengeId: 'challenge',
    approval: {
      operation: 'interactive_apply',
      acknowledge_health: true,
      accept_capabilities: false,
      session_grant: 'remember_capabilities',
    },
  });
  assert.deepEqual(compareAuthorizationArgs('compare-token'), { authorizationToken: 'compare-token' });
  assert.deepEqual(reviewApplyArgs(owner.identity, reviewedRowDecisions), {
    compareIdentity: owner.identity,
    reviewedRowDecisions,
  });
  assert.deepEqual(autoScanApplyAuthorizationArgs(8, 13), { generation: 8, ticketId: 13 });
  assert.deepEqual(applyAuthorizationArgs('apply-token', 73), {
    authorizationToken: 'apply-token', launchId: 73,
  });
  assert.deepEqual(applyAuthorizationArgs('apply-token'), { authorizationToken: 'apply-token' });
});

test('execution payloads cannot contain a client plan or caller-controlled run consent', () => {
  const compare = compareAuthorizationArgs('compare-token');
  const apply = applyAuthorizationArgs('apply-token', 73);
  assert.deepEqual(Object.keys(compare), ['authorizationToken']);
  assert.deepEqual(Object.keys(apply), ['authorizationToken', 'launchId']);
  for (const payload of [compare, apply]) {
    assert.equal('plan' in payload, false);
    assert.equal('reviewedRowDecisions' in payload, false);
    assert.equal('selected' in payload, false);
    assert.equal('acknowledged' in payload, false);
    assert.equal('acceptCaps' in payload, false);
  }
});
