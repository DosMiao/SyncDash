import {
  defaultCompareWorkspacePreferences,
  type CompareWorkspacePreferences,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import { preferenceErrorMessage, type PreferenceStorage } from './preferenceStorage.ts';

export interface CompareWorkspacePreferenceLoad {
  preferences: CompareWorkspacePreferences;
  warning: string | null;
}

const PREFERENCES_KEY = 'sd.compare-workspace-preferences.v1';

function validatePreferences(value: unknown): CompareWorkspacePreferences | null {
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  if (typeof record.grouped !== 'boolean') return null;
  if (record.pathMode !== 'relative' && record.pathMode !== 'full') return null;
  if (typeof record.scopePanelCollapsed !== 'boolean') return null;
  return {
    grouped: record.grouped,
    pathMode: record.pathMode,
    scopePanelCollapsed: record.scopePanelCollapsed,
  };
}

export function loadCompareWorkspacePreferences(
  storage: PreferenceStorage,
): CompareWorkspacePreferenceLoad {
  try {
    const stored = storage.getItem(PREFERENCES_KEY);
    if (stored !== null) {
      const preferences = validatePreferences(JSON.parse(stored));
      if (preferences) return { preferences, warning: null };
      return {
        preferences: defaultCompareWorkspacePreferences,
        warning: 'Saved Compare workspace preferences were invalid; defaults are active until you change a preference.',
      };
    }

    const preferences = defaultCompareWorkspacePreferences;
    try {
      storage.setItem(PREFERENCES_KEY, JSON.stringify(preferences));
    } catch (error) {
      return {
        preferences,
        warning: `Default Compare workspace preferences could not be stored: ${preferenceErrorMessage(error)}`,
      };
    }
    return { preferences, warning: null };
  } catch (error) {
    return {
      preferences: defaultCompareWorkspacePreferences,
      warning: `Compare workspace preferences could not be loaded: ${preferenceErrorMessage(error)}`,
    };
  }
}

export function saveCompareWorkspacePreferences(
  storage: PreferenceStorage,
  preferences: CompareWorkspacePreferences,
): string | null {
  try {
    storage.setItem(PREFERENCES_KEY, JSON.stringify(preferences));
    return null;
  } catch (error) {
    return `Compare workspace preferences could not be saved: ${preferenceErrorMessage(error)}`;
  }
}
