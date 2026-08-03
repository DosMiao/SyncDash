import type { WhenFinishedAction } from '#core/application/progress/postRunActions.ts';
import {
  preferenceErrorMessage,
  type PreferenceStorage,
  type PreferenceStorageWriter,
} from './preferenceStorage.ts';

export type { WhenFinishedAction };

export interface StoredProgressPreferences {
  autoCloseEnabled: boolean;
  whenFinishedAction: WhenFinishedAction;
  failures: string[];
}

const AUTO_CLOSE_PREFERENCE_KEY = 'sd.autoclose';
export const WHEN_FINISHED_PREFERENCE_KEY = 'sd.progress.when-finished.v1';

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
    failures.push(`Could not load the Auto-close preference: ${preferenceErrorMessage(error)}`);
  }

  try {
    const storedWhenFinished = storage.getItem(WHEN_FINISHED_PREFERENCE_KEY);
    if (storedWhenFinished !== null) {
      if (isWhenFinishedAction(storedWhenFinished)) whenFinishedAction = storedWhenFinished;
      else failures.push('When-finished preference is invalid and was ignored');
    }
  } catch (error) {
    failures.push(`Could not load the When-finished preference: ${preferenceErrorMessage(error)}`);
  }
  return { autoCloseEnabled, whenFinishedAction, failures };
}

function writePreference(
  storage: PreferenceStorageWriter,
  key: string,
  value: string,
): string | null {
  try {
    storage.setItem(key, value);
    return null;
  } catch (error) {
    return preferenceErrorMessage(error);
  }
}

export function saveAutoClosePreference(
  storage: PreferenceStorageWriter,
  enabled: boolean,
): string | null {
  return writePreference(storage, AUTO_CLOSE_PREFERENCE_KEY, enabled ? '1' : '0');
}

export function saveWhenFinishedPreference(
  storage: PreferenceStorageWriter,
  action: WhenFinishedAction,
): string | null {
  return writePreference(storage, WHEN_FINISHED_PREFERENCE_KEY, action);
}
