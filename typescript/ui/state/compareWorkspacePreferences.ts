import {
  defaultCompareWorkspacePreferences,
  type CompareWorkspacePreferences,
} from './compareWorkspaceModel.ts';

interface PreferenceStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface CompareWorkspacePreferenceLoad {
  preferences: CompareWorkspacePreferences;
  warning: string | null;
}

const PREFERENCES_KEY = 'sd.compare-workspace-preferences.v1';
const LEGACY_GROUPING_KEY = 'sd.grouped';
const LEGACY_PATH_MODE_KEY = 'sd.pathmode';
const LEGACY_RUN_SCOPE_PANEL_KEY = 'sd.scope';
const LEGACY_OVERVIEW_PANEL_KEY = 'sd.ov';

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

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
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

    const legacyScopePanel = storage.getItem(LEGACY_RUN_SCOPE_PANEL_KEY)
      ?? storage.getItem(LEGACY_OVERVIEW_PANEL_KEY);
    const preferences: CompareWorkspacePreferences = {
      grouped: storage.getItem(LEGACY_GROUPING_KEY) !== 'off',
      pathMode: storage.getItem(LEGACY_PATH_MODE_KEY) === 'full' ? 'full' : 'relative',
      scopePanelCollapsed: legacyScopePanel !== 'open',
    };
    try {
      storage.setItem(PREFERENCES_KEY, JSON.stringify(preferences));
      for (const key of [
        LEGACY_GROUPING_KEY,
        LEGACY_PATH_MODE_KEY,
        LEGACY_RUN_SCOPE_PANEL_KEY,
        LEGACY_OVERVIEW_PANEL_KEY,
      ]) storage.removeItem(key);
    } catch (error) {
      return {
        preferences,
        warning: `Compare workspace preferences were loaded but could not be migrated: ${errorMessage(error)}`,
      };
    }
    return { preferences, warning: null };
  } catch (error) {
    return {
      preferences: defaultCompareWorkspacePreferences,
      warning: `Compare workspace preferences could not be loaded: ${errorMessage(error)}`,
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
    return `Compare workspace preferences could not be saved: ${errorMessage(error)}`;
  }
}
