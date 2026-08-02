import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';

import { formToJob, jobToForm } from '../../typescript/core/formSchema.ts';
import type { Job } from '../../typescript/core/types/generated/Job.ts';

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

test('job mutations have one imperative fence and cannot be dismissed in flight', async () => {
  const source = await readFile(
    new URL('../../typescript/ui/components/JobEditor.tsx', import.meta.url),
    'utf8',
  );
  assert.match(source, /const mutationInFlight = useRef\(false\)/);
  assert.match(source, /if \(mutationInFlight\.current \|\| busy\) return false/);
  assert.match(source, /onClose=\{requestClose\}/);
  assert.match(source, /disabled=\{mutationKind !== null\} onClick=\{requestClose\}/);
  assert.doesNotMatch(source, /<button(?![^>]*\btype=)[^>]*>/);
});

test('job editor fences stale loads and pickers and confirms dirty dismissal', async () => {
  const source = await readFile(
    new URL('../../typescript/ui/components/JobEditor.tsx', import.meta.url),
    'utf8',
  );

  assert.match(source, /loadRequestId\.current !== requestId/);
  assert.match(source, /if \(!mounted\.current \|\| pickerRequest\.current !== request\) return/);
  assert.match(source, /fieldRevisions\.current\.get\(key\).*request\.fieldRevision/);
  assert.match(source, /const formDirty = !!loadedForm && !sameFormValues/);
  assert.match(source, /setDiscardConfirmationOpen\(true\)/);
  assert.match(source, /Discard unsaved changes/);
  assert.match(source, /cancelAnimationFrame\(validationFocusFrame\.current\)/);
});
