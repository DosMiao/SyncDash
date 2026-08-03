import assert from 'node:assert/strict';
import test from 'node:test';

import { StatusAuthority } from '#ui/shared/status/statusAuthority.ts';
import { deferred, flushMicrotasks } from './asyncSettlement.mts';

test('status action execution is single-flight and an unsuperseded failure remains retryable', async () => {
  const authority = new StatusAuthority();
  const request = deferred();
  let executionCount = 0;
  authority.offerAction('Root changed', 'Undo root', () => {
    executionCount += 1;
    return request.promise;
  });

  authority.executeAction();
  authority.executeAction();
  assert.equal(executionCount, 1);
  assert.equal(authority.getState().actionPending, true);
  request.reject(new Error('disk unavailable'));
  await flushMicrotasks();

  assert.equal(authority.getState().message, 'Undo root failed: Error: disk unavailable');
  assert.equal(authority.getState().appearance, 'err');
  assert.equal(authority.getState().action?.label, 'Undo root');
  assert.equal(authority.getState().actionPending, false);
});

test('a superseded failure becomes a separate retryable notice without rewriting newer status', async () => {
  const authority = new StatusAuthority();
  const firstRequest = deferred();
  let activeRequest = firstRequest;
  authority.offerAction('Root changed', 'Undo root', () => activeRequest.promise);
  authority.executeAction();
  authority.setMessage('Compare finished', 'ok');
  firstRequest.reject('write conflict');
  await flushMicrotasks();

  const failedState = authority.getState();
  assert.equal(failedState.message, 'Compare finished');
  assert.equal(failedState.appearance, 'ok');
  assert.equal(failedState.notices.length, 1);
  assert.equal(failedState.notices[0]?.message, 'Undo root failed: write conflict');
  assert.equal(failedState.notices[0]?.action?.label, 'Undo root');

  const retryRequest = deferred();
  activeRequest = retryRequest;
  authority.executeAction(failedState.notices[0]!.action!.id);
  authority.dismissNotice(failedState.notices[0]!.id);
  assert.equal(authority.getState().notices.length, 1);
  retryRequest.resolve();
  await flushMicrotasks();
  assert.equal(authority.getState().notices.length, 0);
  assert.equal(authority.getState().message, 'Compare finished');
});

test('notice retry failure updates only that notice', async () => {
  const authority = new StatusAuthority();
  const initialRequest = deferred();
  let activeRequest = initialRequest;
  authority.offerAction('Changed', 'Undo', () => activeRequest.promise);
  authority.executeAction();
  authority.setMessage('New status');
  initialRequest.reject('first failure');
  await flushMicrotasks();

  const notice = authority.getState().notices[0]!;
  const retryRequest = deferred();
  activeRequest = retryRequest;
  authority.executeAction(notice.action!.id);
  authority.setMessage('Newest status', 'ok');
  retryRequest.reject('retry failure');
  await flushMicrotasks();

  assert.equal(authority.getState().message, 'Newest status');
  assert.equal(authority.getState().appearance, 'ok');
  assert.equal(authority.getState().notices[0]?.message, 'Undo failed: retry failure');
  assert.equal(authority.getState().notices[0]?.action?.id, notice.action!.id);
});
