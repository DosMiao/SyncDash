import assert from 'node:assert/strict';
import test from 'node:test';

import { SerialRequestQueue } from '#core/application/coordination/serialRequestQueue.ts';

test('serial request queue preserves invocation order across reordered completions', async () => {
  const queue = new SerialRequestQueue();
  const events: string[] = [];
  let releaseFirst!: () => void;
  const firstGate = new Promise<void>((resolve) => { releaseFirst = resolve; });

  const first = queue.run(async () => {
    events.push('first-start');
    await firstGate;
    events.push('first-end');
    return 1;
  });
  const second = queue.run(async () => {
    events.push('second-start');
    events.push('second-end');
    return 2;
  });

  await Promise.resolve();
  assert.deepEqual(events, ['first-start']);
  releaseFirst();
  assert.deepEqual(await Promise.all([first, second]), [1, 2]);
  assert.deepEqual(events, ['first-start', 'first-end', 'second-start', 'second-end']);
});

test('a failed request releases the next request', async () => {
  const queue = new SerialRequestQueue();
  const failed = queue.run(async () => { throw new Error('first failed'); });
  const succeeded = queue.run(async () => 'second completed');

  await assert.rejects(failed, /first failed/);
  assert.equal(await succeeded, 'second completed');
});
