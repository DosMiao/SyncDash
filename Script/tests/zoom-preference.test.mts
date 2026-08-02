import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

import {
  ZoomAuthority,
  createInitialZoomAuthorityState,
  loadZoomPreference,
  saveZoomPreference,
  stepZoom,
} from '../../typescript/core/zoom.ts';
import type {
  ZoomAuthorityFailure,
  ZoomAuthorityState,
  ZoomFactor,
} from '../../typescript/core/zoom.ts';

function storage(value: string | null): Storage {
  return {
    getItem: () => value,
    setItem: () => {},
    removeItem: () => {},
    clear: () => {},
    key: () => null,
    length: value === null ? 0 : 1,
  };
}

interface Deferred {
  promise: Promise<void>;
  resolve(): void;
  reject(error: unknown): void;
}

function deferred(): Deferred {
  let resolvePromise!: () => void;
  let rejectPromise!: (error: unknown) => void;
  const promise = new Promise<void>((resolve, reject) => {
    resolvePromise = resolve;
    rejectPromise = reject;
  });
  return { promise, resolve: resolvePromise, reject: rejectPromise };
}

async function flushAsyncWork(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

function initialAuthorityState(): ZoomAuthorityState {
  return createInitialZoomAuthorityState({
    factor: 1,
    persistedFactor: 1,
    warning: null,
  });
}

test('zoom preference accepts only supported discrete factors', () => {
  assert.deepEqual(loadZoomPreference(storage(null)), {
    factor: 1,
    persistedFactor: null,
    warning: null,
  });
  assert.deepEqual(loadZoomPreference(storage('1.25')), {
    factor: 1.25,
    persistedFactor: 1.25,
    warning: null,
  });
  const invalid = loadZoomPreference(storage('1.234'));
  assert.equal(invalid.factor, 1);
  assert.equal(invalid.persistedFactor, null);
  assert.match(invalid.warning ?? '', /invalid interface zoom/i);
});

test('zoom storage failures are explicit and stepping rejects unknown state', () => {
  const failingRead = storage(null);
  failingRead.getItem = () => { throw new Error('read denied'); };
  const loaded = loadZoomPreference(failingRead);
  assert.equal(loaded.factor, 1);
  assert.equal(loaded.persistedFactor, null);
  assert.match(loaded.warning ?? '', /read denied/);

  const failingWrite = storage(null);
  failingWrite.setItem = () => { throw new Error('write denied'); };
  assert.match(saveZoomPreference(1.25, failingWrite) ?? '', /write denied/);
  assert.throws(() => saveZoomPreference(1.234, failingWrite), /Invalid interface zoom factor/);

  assert.equal(stepZoom(0.8, -1), 0.8);
  assert.equal(stepZoom(2, 1), 2);
  assert.throws(() => stepZoom(1.234, 1), /Invalid interface zoom factor/);
});

test('rapid requests coalesce behind a monotonic fence and persist only the latest applied factor', async () => {
  const applications: Array<{ factor: ZoomFactor; completion: Deferred }> = [];
  const persistedFactors: ZoomFactor[] = [];
  const states: ZoomAuthorityState[] = [];
  const failures: ZoomAuthorityFailure[] = [];
  const authority = new ZoomAuthority(initialAuthorityState(), {
    applyWebviewZoom: (factor) => {
      const completion = deferred();
      applications.push({ factor, completion });
      return completion.promise;
    },
    persistPreference: (factor) => {
      persistedFactors.push(factor);
      return null;
    },
    publishState: (state) => states.push(state),
    onFailure: (failure) => failures.push(failure),
  });

  const firstRequestId = authority.requestZoom(1.1, { persist: true, reason: 'change' });
  const secondRequestId = authority.requestZoom(1.25, { persist: true, reason: 'change' });
  assert.ok(secondRequestId > firstRequestId);
  assert.equal(authority.getState().desiredFactor, 1.25);
  assert.deepEqual(applications.map((application) => application.factor), [1.1]);

  applications[0].completion.resolve();
  await flushAsyncWork();
  assert.deepEqual(applications.map((application) => application.factor), [1.1, 1.25]);
  assert.equal(states.some((state) => state.appliedFactor === 1.1), false);
  assert.deepEqual(authority.getState(), {
    desiredFactor: 1.25,
    appliedFactor: 1.1,
    persistedFactor: 1,
    latestRequestId: secondRequestId,
    pending: true,
  });
  assert.deepEqual(persistedFactors, []);

  applications[1].completion.resolve();
  await flushAsyncWork();
  assert.deepEqual(authority.getState(), {
    desiredFactor: 1.25,
    appliedFactor: 1.25,
    persistedFactor: 1.25,
    latestRequestId: secondRequestId,
    pending: false,
  });
  assert.deepEqual(persistedFactors, [1.25]);
  assert.deepEqual(failures, []);
});

test('restoring a persisted factor applies it without rewriting storage', async () => {
  const restoredInitialState = createInitialZoomAuthorityState({
    factor: 1.25,
    persistedFactor: 1.25,
    warning: null,
  });
  let persistenceCalls = 0;
  const authority = new ZoomAuthority(restoredInitialState, {
    applyWebviewZoom: async () => {},
    persistPreference: () => {
      persistenceCalls += 1;
      return null;
    },
    publishState: () => {},
    onFailure: () => {},
  });

  authority.requestZoom(1.25, { persist: false, reason: 'restore' });
  await flushAsyncWork();

  assert.equal(persistenceCalls, 0);
  assert.deepEqual(authority.getState(), {
    desiredFactor: 1.25,
    appliedFactor: 1.25,
    persistedFactor: 1.25,
    latestRequestId: 1,
    pending: false,
  });
});

test('webview failure never persists and stale failure cannot regress or report', async () => {
  const applications: Array<{ factor: ZoomFactor; completion: Deferred }> = [];
  const persistedFactors: ZoomFactor[] = [];
  const failures: ZoomAuthorityFailure[] = [];
  const authority = new ZoomAuthority(initialAuthorityState(), {
    applyWebviewZoom: (factor) => {
      const completion = deferred();
      applications.push({ factor, completion });
      return completion.promise;
    },
    persistPreference: (factor) => {
      persistedFactors.push(factor);
      return null;
    },
    publishState: () => {},
    onFailure: (failure) => failures.push(failure),
  });

  authority.requestZoom(1.1, { persist: true, reason: 'change' });
  authority.requestZoom(1.25, { persist: true, reason: 'change' });
  applications[0].completion.reject(new Error('stale failure'));
  await flushAsyncWork();
  applications[1].completion.reject(new Error('latest failure'));
  await flushAsyncWork();

  assert.deepEqual(persistedFactors, []);
  assert.equal(failures.length, 1);
  assert.equal(failures[0]?.kind, 'application');
  assert.match(String(failures[0]?.error), /latest failure/);
  assert.deepEqual(authority.getState(), {
    desiredFactor: 1,
    appliedFactor: 1,
    persistedFactor: 1,
    latestRequestId: 2,
    pending: false,
  });
});

test('latest failure reconciles to a successful intermediate webview factor', async () => {
  const applications: Deferred[] = [];
  const displayedFactors: ZoomFactor[] = [];
  const authority = new ZoomAuthority(initialAuthorityState(), {
    applyWebviewZoom: () => {
      const completion = deferred();
      applications.push(completion);
      return completion.promise;
    },
    persistPreference: () => null,
    publishState: (state) => displayedFactors.push(state.appliedFactor),
    onFailure: () => {},
  });

  authority.requestZoom(1.1, { persist: true, reason: 'change' });
  authority.requestZoom(1.25, { persist: true, reason: 'change' });
  applications[0].resolve();
  await flushAsyncWork();
  assert.equal(displayedFactors.includes(1.1), false);
  applications[1].reject(new Error('latest failure'));
  await flushAsyncWork();

  assert.equal(authority.getState().desiredFactor, 1.1);
  assert.equal(authority.getState().appliedFactor, 1.1);
  assert.equal(displayedFactors.at(-1), 1.1);
});

test('persistence failure keeps the successfully applied session zoom', async () => {
  const failures: ZoomAuthorityFailure[] = [];
  const authority = new ZoomAuthority(initialAuthorityState(), {
    applyWebviewZoom: async () => {},
    persistPreference: () => 'storage denied',
    publishState: () => {},
    onFailure: (failure) => failures.push(failure),
  });

  authority.requestZoom(1.4, { persist: true, reason: 'change' });
  await flushAsyncWork();

  assert.deepEqual(authority.getState(), {
    desiredFactor: 1.4,
    appliedFactor: 1.4,
    persistedFactor: 1,
    latestRequestId: 1,
    pending: false,
  });
  assert.deepEqual(failures, [{
    kind: 'persistence',
    factor: 1.4,
    reason: 'change',
    error: 'storage denied',
  }]);
});

test('zoom hook steps from desired state and renders only applied state', async () => {
  const source = await readFile(
    new URL('../../typescript/ui/hooks/useZoomControl.ts', import.meta.url),
    'utf8',
  );
  assert.match(source, /stepZoom\(desiredZoomFactorRef\.current, direction\)/);
  assert.match(source, /zoom: authorityState\.appliedFactor/);
  assert.match(source, /persist: false,[\s\S]*reason: 'restore'/);
  assert.match(source, /for this session only; the preference could not be saved/);
});
