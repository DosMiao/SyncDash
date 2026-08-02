import assert from 'node:assert/strict';
import test from 'node:test';

import { endpointInputState } from '#core/domain/paths/endpointReadiness.ts';
import type { PathInfo } from '#core/types/generated/PathInfo.ts';

function info(readiness: PathInfo['readiness']): PathInfo {
  return { readiness, exists: null, is_dir: null, has_marker: null };
}

test('only observed local directories render as ready', () => {
  assert.equal(endpointInputState(info('ready'), '/data'), 'good');
  assert.equal(endpointInputState(info('missing'), '/data'), 'bad');
  assert.equal(endpointInputState(info('not_directory'), '/data'), 'bad');
  assert.equal(endpointInputState(info('invalid'), 'sfpt://host/data'), 'bad');
});

test('deferred and unobservable endpoints never render a fabricated success', () => {
  assert.equal(endpointInputState(info('deferred'), 'sftp://host/data'), '');
  assert.equal(endpointInputState(info('unobservable'), '/protected'), '');
  assert.equal(endpointInputState(info('ready'), ''), '');
});
