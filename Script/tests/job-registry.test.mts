import assert from 'node:assert/strict';
import test from 'node:test';

import {
  emptyJobRegistryState,
  reduceJobRegistry,
  validateJobRegistrySnapshot,
} from '#core/application/jobs/jobRegistry.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';

function job(jobId: string, name: string, configRevision = 'revision-a'): JobDto {
  return {
    job_id: jobId,
    name,
    config_revision: configRevision,
    targets: ['/target'],
  } as JobDto;
}

test('job selection follows stable identity across rename and refreshed fields', () => {
  const original = job('job-a', 'Photos');
  let state = reduceJobRegistry(emptyJobRegistryState, {
    type: 'snapshot_received', jobs: [original],
  });
  state = reduceJobRegistry(state, { type: 'selection_changed', jobId: original.job_id });
  const renamed = job('job-a', 'Archive', 'revision-b');
  state = reduceJobRegistry(state, { type: 'snapshot_received', jobs: [renamed] });

  assert.equal(state.selectedJobId, 'job-a');
  assert.equal(state.jobs[0], renamed);
});

test('deletion and same-name recreation cannot transfer selection authority', () => {
  let state = reduceJobRegistry(emptyJobRegistryState, {
    type: 'snapshot_received', jobs: [job('job-a', 'Photos')],
  });
  state = reduceJobRegistry(state, { type: 'selection_changed', jobId: 'job-a' });
  state = reduceJobRegistry(state, {
    type: 'snapshot_received', jobs: [job('job-b', 'Photos')],
  });

  assert.equal(state.selectedJobId, null);
  assert.equal(state.jobs[0].job_id, 'job-b');
});

test('malformed registry snapshots fail before they can become selection authority', () => {
  assert.throws(
    () => validateJobRegistrySnapshot([job('job-a', 'One'), job('job-a', 'Two')]),
    /duplicate identity/,
  );
  assert.throws(
    () => validateJobRegistrySnapshot([job('job-a', 'Same'), job('job-b', 'Same')]),
    /duplicate name/,
  );
});
