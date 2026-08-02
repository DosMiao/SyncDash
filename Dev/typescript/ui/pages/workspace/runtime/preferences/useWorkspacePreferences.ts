import { useCallback, useEffect, useState } from 'react';
import type { CompareWorkspacePreferences } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import {
  loadCompareWorkspacePreferences,
  saveCompareWorkspacePreferences,
} from '#core/infrastructure/preferences/compareWorkspacePreferences.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

export function useWorkspacePreferences(setStatus: StatusApi['setMessage']) {
  const [initialLoad] = useState(() => loadCompareWorkspacePreferences(localStorage));
  const [preferences, setPreferences] = useState<CompareWorkspacePreferences>(initialLoad.preferences);

  useEffect(() => {
    if (initialLoad.warning) setStatus(initialLoad.warning, 'err');
  }, [initialLoad.warning, setStatus]);

  const persistPreferences = useCallback((next: CompareWorkspacePreferences) => {
    const warning = saveCompareWorkspacePreferences(localStorage, next);
    if (warning) setStatus(warning, 'err');
  }, [setStatus]);

  return { preferences, setPreferences, persistPreferences };
}
