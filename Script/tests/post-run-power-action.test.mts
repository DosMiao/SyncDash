import assert from 'node:assert/strict';
import test from 'node:test';

import {
  deriveAutoCloseRequest,
  derivePowerActionCountdown,
} from '#core/application/progress/postRunActions.ts';

const successful = {
  readyRunId: 7,
  currentRunId: 7,
  summary: { cancelled: false, errors: 0 },
  applying: true,
  autoCloseEnabled: false,
  whenFinishedAction: 'sleep' as const,
};

test('countdown requires the backend grant for the exact clean Apply run', () => {
  assert.deepEqual(derivePowerActionCountdown(successful), {
    action: 'sleep',
    runId: 7,
    secondsRemaining: 10,
  });
  assert.equal(derivePowerActionCountdown({ ...successful, readyRunId: 6 }), null);
  assert.equal(derivePowerActionCountdown({ ...successful, readyRunId: null }), null);
  assert.equal(derivePowerActionCountdown({ ...successful, applying: false }), null);
});

test('cancelled, failed, incomplete, auto-closing, and disabled outcomes fail closed', () => {
  assert.equal(derivePowerActionCountdown({
    ...successful,
    summary: { cancelled: true, errors: 0 },
  }), null);
  assert.equal(derivePowerActionCountdown({
    ...successful,
    summary: { cancelled: false, errors: 1 },
  }), null);
  assert.equal(derivePowerActionCountdown({
    ...successful,
    summary: { cancelled: false },
  }), null);
  assert.equal(derivePowerActionCountdown({ ...successful, autoCloseEnabled: true }), null);
  assert.equal(derivePowerActionCountdown({ ...successful, whenFinishedAction: 'none' }), null);
});

const autoCloseFacts = {
  completedRunId: 7,
  currentRunId: 7,
  summary: { cancelled: false, errors: 0 },
  applying: true,
  autoCloseEnabled: true,
  closeAfterStop: false,
};

test('auto-close is reserved only for the exact clean completed Apply run', () => {
  assert.deepEqual(deriveAutoCloseRequest(autoCloseFacts), { runId: 7 });
  assert.equal(deriveAutoCloseRequest({ ...autoCloseFacts, completedRunId: 6 }), null);
  assert.equal(deriveAutoCloseRequest({ ...autoCloseFacts, currentRunId: 8 }), null);
  assert.equal(deriveAutoCloseRequest({ ...autoCloseFacts, applying: false }), null);
  assert.equal(deriveAutoCloseRequest({ ...autoCloseFacts, autoCloseEnabled: false }), null);
  assert.equal(deriveAutoCloseRequest({ ...autoCloseFacts, closeAfterStop: true }), null);
  assert.equal(deriveAutoCloseRequest({
    ...autoCloseFacts,
    summary: { cancelled: true, errors: 0 },
  }), null);
  assert.equal(deriveAutoCloseRequest({
    ...autoCloseFacts,
    summary: { cancelled: false, errors: 1 },
  }), null);
});
