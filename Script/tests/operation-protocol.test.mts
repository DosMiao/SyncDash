import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';

import {
  applyAuthorizationArgs,
  approveOperationArgs,
  autoScanApplyAuthorizationArgs,
  compareAuthorizationArgs,
  reviewApplyArgs,
  reviewCompareArgs,
  startAutoScanArgs,
} from '../../typescript/core/operation-protocol.ts';
import type { CompareOwner } from '../../typescript/core/types/generated/CompareOwner.ts';

const owner: CompareOwner = {
  compare_id: 41,
  job_id: 'job-stable-id',
  job_name: 'photos',
  config_revision: 'revision-7',
  target_index: 2,
};
const selected = [{ index: 3, flipped: false }, { index: 9, flipped: true }];

test('Tauri argument mapping uses camelCase only at the boundary', () => {
  assert.deepEqual(startAutoScanArgs('job-stable-id', 'revision-7', 2), {
    expectedJobId: 'job-stable-id', expectedRevision: 'revision-7', targetIndex: 2,
  });
  assert.deepEqual(reviewCompareArgs('job-stable-id', 2), {
    expectedJobId: 'job-stable-id', targetIndex: 2,
  });
  assert.deepEqual(reviewCompareArgs('job-stable-id'), { expectedJobId: 'job-stable-id' });
  assert.deepEqual(approveOperationArgs('challenge', true, false, true, false), {
    challengeId: 'challenge',
    acknowledgeHealth: true,
    acceptCapabilities: false,
    rememberForSession: true,
    allowUnattended: false,
  });
  assert.deepEqual(compareAuthorizationArgs('compare-token'), { authorizationToken: 'compare-token' });
  assert.deepEqual(reviewApplyArgs(owner, selected), { owner, selected });
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
    assert.equal('selected' in payload, false);
    assert.equal('acknowledged' in payload, false);
    assert.equal('acceptCaps' in payload, false);
  }
});

test('frontend source has no legacy confirmation or raw-consent execution path', async () => {
  const ipc = await readFile(new URL('../../typescript/core/ipc.ts', import.meta.url), 'utf8');
  const app = await readFile(new URL('../../typescript/ui/App.tsx', import.meta.url), 'utf8');
  const executionSources = `${ipc}\n${app}`;
  assert.doesNotMatch(executionSources, /window\.confirm\s*\(/);
  assert.doesNotMatch(executionSources, /withCapsConsent|capsConsent|acceptCaps/);
  assert.doesNotMatch(executionSources, /applyJobUnattended|ipc\.preflight|preflightAllowsApply/);
  assert.doesNotMatch(executionSources, /authorizeUnattendedApply|authorize_unattended_apply/);
  assert.doesNotMatch(executionSources, /authorizeAutoScanApply\s*\([^)]*(owner|selected)/);
  assert.doesNotMatch(ipc, /invoke<ApplyDto>\('apply_job',\s*\{[^}]*\b(plan|selected|acknowledged)\b/s);
  assert.match(ipc, /invoke<ApplyDto>\('apply_job', applyAuthorizationArgs\(/);
  assert.match(ipc, /invoke<PlanDto>\('compare_job', compareAuthorizationArgs\(/);
});
