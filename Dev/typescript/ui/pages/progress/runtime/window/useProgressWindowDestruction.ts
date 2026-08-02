import { useCallback, useRef } from 'react';
import { progressPlatformActions } from '../actions/progressPlatformActions.ts';

/** Fences irreversible progress-window destruction behind one in-flight request. */
export function useProgressWindowDestruction(
  reportControlError: (action: string, error: unknown) => void,
) {
  const closeRequestPendingRef = useRef(false);
  const windowDestructionPendingRef = useRef(false);

  const requestWindowDestruction = useCallback(async () => {
    if (windowDestructionPendingRef.current) return;
    windowDestructionPendingRef.current = true;
    try {
      await progressPlatformActions.destroyWindow();
    } catch (error) {
      windowDestructionPendingRef.current = false;
      reportControlError('Could not close the progress window', error);
    }
  }, [reportControlError]);

  return {
    closeRequestPendingRef,
    requestWindowDestruction,
    windowDestructionPendingRef,
  };
}
