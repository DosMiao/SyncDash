import { useCallback, useRef, useState } from 'react';
import type { MutableRefObject } from 'react';
import type { RunState } from '#core/application/progress/runstate.ts';

/** Publishes runtime failures into the run record and owns error-detail presentation state. */
export function useProgressErrorState(
  runStateRef: MutableRefObject<RunState>,
  requestRender: () => void,
) {
  const reportedWindowChromeFailuresRef = useRef(new Set<string>());
  const [errorDetailsOpen, setErrorDetailsOpen] = useState(false);

  const reportControlError = useCallback((action: string, error: unknown) => {
    runStateRef.current.errors.push({
      path: '', action: 'control', side: '', message: `${action}: ${String(error)}`, warning: false,
    });
    setErrorDetailsOpen(true);
    requestRender();
  }, [requestRender, runStateRef]);

  const reportWindowChromeFailure = useCallback((action: string, error: unknown) => {
    if (reportedWindowChromeFailuresRef.current.has(action)) return;
    reportedWindowChromeFailuresRef.current.add(action);
    runStateRef.current.errors.push({
      path: '', action: 'window', side: '', message: `${action}: ${String(error)}`, warning: true,
    });
    requestRender();
  }, [requestRender, runStateRef]);

  const toggleErrorDetails = useCallback(() => setErrorDetailsOpen((open) => !open), []);

  return {
    errorDetailsOpen,
    reportControlError,
    reportWindowChromeFailure,
    toggleErrorDetails,
  };
}
