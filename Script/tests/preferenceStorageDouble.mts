// Browser-storage doubles shared by the preference suites. Not a `*.test.mts` file, so the
// `node --test Script/tests/*.test.mts` run does not treat it as a suite.

export interface PreferenceStorageDouble {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export interface MemoryStorageDouble extends PreferenceStorageDouble {
  values: Map<string, string>;
  writes: Array<[string, string]>;
}

export function memoryStorage(initial: Record<string, string> = {}): MemoryStorageDouble {
  const values = new Map(Object.entries(initial));
  const writes: Array<[string, string]> = [];
  return {
    values,
    writes,
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => {
      writes.push([key, value]);
      values.set(key, value);
    },
  };
}

/** Storage that refuses every operation, so a suite can assert the failure reaches the caller. */
export function denyingStorage(message: string): PreferenceStorageDouble {
  const deny = (): never => { throw new Error(message); };
  return { getItem: deny, setItem: deny };
}
