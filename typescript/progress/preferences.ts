export type WhenFinishedAction = 'none' | 'sleep' | 'shutdown';

interface PreferenceStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

interface StorageWriter {
  setItem(key: string, value: string): void;
}

export interface StoredProgressPreferences {
  autoCloseEnabled: boolean;
  whenFinishedAction: WhenFinishedAction;
  failures: string[];
}

const AUTO_CLOSE_PREFERENCE_KEY = 'sd.autoclose';
export const WHEN_FINISHED_PREFERENCE_KEY = 'sd.progress.when-finished.v1';
const LEGACY_WHEN_FINISHED_PREFERENCE_KEY = 'sd.whenfin';

export function isWhenFinishedAction(value: string): value is WhenFinishedAction {
  return value === 'none' || value === 'sleep' || value === 'shutdown';
}

export function loadProgressPreferences(storage: PreferenceStorage): StoredProgressPreferences {
  const failures: string[] = [];
  let autoCloseEnabled = false;
  let whenFinishedAction: WhenFinishedAction = 'none';
  try {
    const storedAutoClose = storage.getItem(AUTO_CLOSE_PREFERENCE_KEY);
    if (storedAutoClose === '1') autoCloseEnabled = true;
    else if (storedAutoClose !== null && storedAutoClose !== '0') {
      failures.push('Auto-close preference is invalid and was ignored');
    }
  } catch (error) {
    failures.push(`Could not load the Auto-close preference: ${String(error)}`);
  }

  try {
    const storedWhenFinished = storage.getItem(WHEN_FINISHED_PREFERENCE_KEY);
    if (storedWhenFinished !== null) {
      if (isWhenFinishedAction(storedWhenFinished)) {
        whenFinishedAction = storedWhenFinished;
        try {
          storage.removeItem(LEGACY_WHEN_FINISHED_PREFERENCE_KEY);
        } catch (error) {
          failures.push(`Could not remove the legacy When-finished preference: ${String(error)}`);
        }
      } else {
        failures.push('When-finished preference is invalid and was ignored');
      }
    } else {
      const legacyWhenFinished = storage.getItem(LEGACY_WHEN_FINISHED_PREFERENCE_KEY);
      if (legacyWhenFinished !== null && isWhenFinishedAction(legacyWhenFinished)) {
        whenFinishedAction = legacyWhenFinished;
        try {
          storage.setItem(WHEN_FINISHED_PREFERENCE_KEY, legacyWhenFinished);
          storage.removeItem(LEGACY_WHEN_FINISHED_PREFERENCE_KEY);
        } catch (error) {
          failures.push(`Could not migrate the When-finished preference: ${String(error)}`);
        }
      } else if (legacyWhenFinished !== null) {
        failures.push('Legacy When-finished preference is invalid and was ignored');
      }
    }
  } catch (error) {
    failures.push(`Could not load the When-finished preference: ${String(error)}`);
  }
  return { autoCloseEnabled, whenFinishedAction, failures };
}

function writePreference(storage: StorageWriter, key: string, value: string): string | null {
  try {
    storage.setItem(key, value);
    return null;
  } catch (error) {
    return String(error);
  }
}

export function saveAutoClosePreference(storage: StorageWriter, enabled: boolean): string | null {
  return writePreference(storage, AUTO_CLOSE_PREFERENCE_KEY, enabled ? '1' : '0');
}

export function saveWhenFinishedPreference(
  storage: StorageWriter,
  action: WhenFinishedAction,
): string | null {
  return writePreference(storage, WHEN_FINISHED_PREFERENCE_KEY, action);
}
