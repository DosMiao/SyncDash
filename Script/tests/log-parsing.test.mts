import assert from 'node:assert/strict';
import test from 'node:test';

import { parseLogArtifactLine } from '#core/domain/runs/logs.ts';

test('legacy text remains visible while malformed JSON is explicit', () => {
  assert.deepEqual(parseLogArtifactLine('old plain-text event', 'run'), {
    timestampMs: 0,
    level: 'info',
    scope: 'legacy',
    message: 'old plain-text event',
    searchText: 'old plain-text event',
  });

  const malformed = parseLogArtifactLine('{"kind":"log"', 'run');
  assert.equal(malformed.level, 'error');
  assert.equal(malformed.scope, 'parse');
  assert.match(malformed.message, /^Malformed JSON log record:/);
});

test('unknown log levels become an explicit malformed-record row', () => {
  const parsed = parseLogArtifactLine(
    JSON.stringify({ kind: 'log', ts_ms: 9, level: 'fatal', scope: 'engine', message: 'x' }),
    'run',
  );
  assert.equal(parsed.level, 'error');
  assert.equal(parsed.scope, 'parse');
  assert.match(parsed.message, /not a supported log level/);
});

test('non-object JSON records produce a visible parse error', () => {
  const parsed = parseLogArtifactLine('null', 'items');
  assert.equal(parsed.level, 'error');
  assert.match(parsed.message, /must be a JSON object/);
});

test('unknown event kinds and structurally invalid numeric fields fail visibly', () => {
  const unknown = parseLogArtifactLine(JSON.stringify({ kind: 'mystery', ts_ms: 1 }), 'run');
  assert.equal(unknown.level, 'error');
  assert.match(unknown.message, /unsupported event kind/);

  const unsafe = parseLogArtifactLine(JSON.stringify({
    kind: 'summary',
    ts_ms: 1,
    done: -1,
    skipped: 0,
    errors: 0,
    bytes_done: 0,
    elapsed_ms: 0,
    paused_ms: 0,
    cancelled: false,
  }), 'run');
  assert.equal(unsafe.level, 'error');
  assert.match(unsafe.message, /non-negative safe integer/);
});
