import assert from 'node:assert/strict';
import test from 'node:test';

import { RequestFence } from '../../typescript/ui/state/request-fence.ts';

test('only the newest request for an owner may publish', () => {
  const fence = new RequestFence();
  const first = fence.start('job-a');
  const second = fence.start('job-a');

  assert.equal(fence.owns(first), false);
  assert.equal(fence.owns(second), true);
});

test('changing owner or invalidating retires every earlier response', () => {
  const fence = new RequestFence();
  const oldOwner = fence.start('job-a');
  const current = fence.start('job-b');

  assert.equal(fence.owns(oldOwner), false);
  assert.equal(fence.owns(current), true);
  fence.invalidate();
  assert.equal(fence.owns(current), false);
});

test('owner-scoped invalidation retires only the matching in-flight request', () => {
  const fence = new RequestFence();
  const current = fence.start('job-b');

  assert.equal(fence.invalidateOwner('job-a'), false);
  assert.equal(fence.owns(current), true);
  assert.equal(fence.invalidateOwner('job-b'), true);
  assert.equal(fence.owns(current), false);
});
