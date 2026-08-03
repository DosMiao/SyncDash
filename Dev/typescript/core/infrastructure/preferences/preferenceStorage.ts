// The browser-storage boundary every durable preference module depends on, and the single way a
// failure at that boundary is described. Reader and writer are separate so a module that only
// persists cannot quietly start reading, and vice versa.

export interface PreferenceStorageReader {
  getItem(key: string): string | null;
}

export interface PreferenceStorageWriter {
  setItem(key: string, value: string): void;
}

export interface PreferenceStorage extends PreferenceStorageReader, PreferenceStorageWriter {}

/**
 * `String(error)` on an `Error` prefixes the constructor name, which reads as noise inside a
 * preference warning. Every preference failure quotes the message alone.
 */
export function preferenceErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
