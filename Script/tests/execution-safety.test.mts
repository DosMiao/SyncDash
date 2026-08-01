import assert from 'node:assert/strict';
import test from 'node:test';

import {
  ownsFreshAutoScanResult,
  rootEditKeyAction,
} from '../../typescript/ui/state/execution-safety.ts';
import type { AutoScanTicket } from '../../typescript/ui/state/execution-safety.ts';
import type { CompareOwner } from '../../typescript/core/types/generated/CompareOwner.ts';

const owner: CompareOwner = {
  compare_id: 17,
  job_id: 'job-id-photos',
  job_name: 'photos',
  config_revision: 'rev-2',
  target_index: 1,
};

test('auto-apply accepts only the result owned by the currently armed cycle and selection', () => {
  const ticket: AutoScanTicket = {
    generation: 9,
    ticketId: 3,
    jobId: 'job-id-photos',
    jobName: 'photos',
    configRevision: 'rev-2',
    targetIndex: 1,
    autoApply: true,
  };
  const selection = { jobId: 'job-id-photos', configRevision: 'rev-2', targetIndex: 1 };

  assert.equal(ownsFreshAutoScanResult(true, ticket, ticket, owner, selection), true);
  assert.equal(ownsFreshAutoScanResult(false, ticket, ticket, owner, selection), false);
  assert.equal(ownsFreshAutoScanResult(true, null, ticket, owner, selection), false);
  assert.equal(ownsFreshAutoScanResult(true, { ...ticket, generation: 10 }, ticket, owner, selection), false);
  assert.equal(ownsFreshAutoScanResult(true, { ...ticket, ticketId: 4 }, ticket, owner, selection), false);
  assert.equal(ownsFreshAutoScanResult(true, ticket, ticket, { ...owner, compare_id: 3, config_revision: 'old' }, selection), false);
  assert.equal(ownsFreshAutoScanResult(true, ticket, ticket, { ...owner, job_id: 'replacement-id' }, selection), false);
  assert.equal(ownsFreshAutoScanResult(true, ticket, ticket, owner, { ...selection, jobId: 'replacement-id' }), false);
  assert.equal(ownsFreshAutoScanResult(true, ticket, ticket, owner, { ...selection, targetIndex: 0 }), false);
});

test('Escape reverts a root edit while Enter is the only commit key', () => {
  assert.equal(rootEditKeyAction('Escape'), 'revert');
  assert.equal(rootEditKeyAction('Enter'), 'commit');
  assert.equal(rootEditKeyAction('Tab'), null);
});
