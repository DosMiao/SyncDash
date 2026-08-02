import { useCallback, useEffect, useRef, useState } from 'react';
import type { ZoomPreferenceLoad } from '#core/application/zoom/zoomAuthority.ts';
import type { WhenFinishedAction } from '#core/application/progress/postRunActions.ts';
import { loadZoomPreference } from '#core/infrastructure/preferences/zoomPreference.ts';
import {
  isWhenFinishedAction,
  loadProgressPreferences,
  saveAutoClosePreference,
  saveWhenFinishedPreference,
} from '#core/infrastructure/preferences/progressPreferences.ts';

type ErrorReporter = (action: string, error: unknown) => void;

/** Owns persisted progress-window choices and their live async-safe mirrors. */
export function useProgressPreferences(reportError: ErrorReporter) {
  const [stored] = useState(() => loadProgressPreferences(localStorage));
  const [zoomPreference] = useState<ZoomPreferenceLoad>(() => loadZoomPreference(localStorage));
  const [autoCloseEnabled, setAutoCloseEnabled] = useState(stored.autoCloseEnabled);
  const [whenFinishedAction, setWhenFinishedAction] = useState<WhenFinishedAction>(stored.whenFinishedAction);
  const autoCloseEnabledRef = useRef(autoCloseEnabled);
  const whenFinishedActionRef = useRef(whenFinishedAction);
  autoCloseEnabledRef.current = autoCloseEnabled;
  whenFinishedActionRef.current = whenFinishedAction;

  useEffect(() => {
    for (const failure of stored.failures) {
      reportError('Could not restore progress preferences', failure);
    }
  }, [reportError, stored.failures]);

  const changeAutoCloseEnabled = useCallback((next: boolean): boolean => {
    const error = saveAutoClosePreference(localStorage, next);
    if (error) {
      reportError('Could not save the Auto-close preference', error);
      return false;
    }
    autoCloseEnabledRef.current = next;
    setAutoCloseEnabled(next);
    return true;
  }, [reportError]);

  const changeWhenFinishedAction = useCallback((next: string): WhenFinishedAction | null => {
    if (!isWhenFinishedAction(next)) {
      reportError('Could not save the When-finished preference', `unknown action: ${next}`);
      return null;
    }
    const error = saveWhenFinishedPreference(localStorage, next);
    if (error) {
      reportError('Could not save the When-finished preference', error);
      return null;
    }
    whenFinishedActionRef.current = next;
    setWhenFinishedAction(next);
    return next;
  }, [reportError]);

  return {
    autoCloseEnabled,
    autoCloseEnabledRef,
    changeAutoCloseEnabled,
    changeWhenFinishedAction,
    whenFinishedAction,
    whenFinishedActionRef,
    zoomPreference,
  };
}
