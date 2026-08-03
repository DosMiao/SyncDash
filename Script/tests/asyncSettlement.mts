// Promise-settlement controls shared by the suites that drive single-flight authorities. Not a
// `*.test.mts` file, so the `node --test Script/tests/*.test.mts` run does not treat it as a suite.

export interface Deferred {
  promise: Promise<void>;
  resolve(): void;
  reject(error: unknown): void;
}

export function deferred(): Deferred {
  let resolvePromise!: () => void;
  let rejectPromise!: (error: unknown) => void;
  const promise = new Promise<void>((resolve, reject) => {
    resolvePromise = resolve;
    rejectPromise = reject;
  });
  return { promise, resolve: resolvePromise, reject: rejectPromise };
}

/** Two turns is what an `await`ed callback chain needs before its observable effect lands. */
export async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}
