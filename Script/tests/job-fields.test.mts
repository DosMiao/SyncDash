import assert from 'node:assert/strict';
import test from 'node:test';

import { formToJob, jobToForm } from '#core/domain/jobs/formSchema.ts';
import type { Job } from '#core/types/generated/Job.ts';

function job(): Job {
  return {
    schema: 4,
    mode: 'mirror',
    source: '/source',
    targets: ['/target'],
    rigor: 'standard',
    evidence: 'sampled',
    use_cache: false,
    escalate: true,
    verify_writes: true,
    autoscan_interval_secs: 30,
    autoscan_auto_apply: false,
  } as unknown as Job;
}

test('job form writes only the canonical target list', () => {
  const base = job();
  const values = jobToForm(base, 'Photos');
  values.targets = ' /backup-one \n\n /backup-two ';
  const result = formToJob(values, base);
  assert.ok(!('error' in result));
  assert.deepEqual(result.job.targets, ['/backup-one', '/backup-two']);
  assert.equal('target' in result.job, false);
  assert.equal(result.job.autoscan_interval_secs, 30);
});

test('job form refuses an empty target list before IPC', () => {
  const base = job();
  const values = jobToForm(base, 'Photos');
  values.targets = ' \n ';
  const result = formToJob(values, base);
  assert.deepEqual(result, {
    error: 'At least one target root is required',
    field: 'targets',
  });
});
