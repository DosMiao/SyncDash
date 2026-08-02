import { useCallback, useRef, useState } from 'react';
import type { MutableRefObject } from 'react';
import { newRunState } from '#core/application/progress/runstate.ts';
import type { RunState } from '#core/application/progress/runstate.ts';
import { progressPlatformActions } from '../actions/progressPlatformActions.ts';
import type { PendingLaunch } from '../progressRuntimeTypes.ts';

interface ProgressLaunchLifecycleOptions {
  runStateRef: MutableRefObject<RunState>;
  requestRender: () => void;
  resetRunControls: () => void;
  resetPowerActions: () => void;
  reportControlError: (action: string, error: unknown) => void;
}

/** Binds one desktop launch identity to the next strictly newer Apply run. */
export function useProgressLaunchLifecycle({
  runStateRef,
  requestRender,
  resetRunControls,
  resetPowerActions,
  reportControlError,
}: ProgressLaunchLifecycleOptions) {
  const pendingLaunchRef = useRef<PendingLaunch | null>(null);
  const [runRejectionMessage, setRunRejectionMessage] = useState<string | null>(null);

  const armLaunch = useCallback((launchId: number) => {
    const afterRunId = runStateRef.current.runId;
    pendingLaunchRef.current = { launchId, afterRunId };
    const resetRunState = newRunState();
    resetRunState.runId = afterRunId;
    runStateRef.current = resetRunState;
    setRunRejectionMessage(null);
    resetRunControls();
    resetPowerActions();
    requestRender();
    void progressPlatformActions.acknowledgeLaunch(launchId).catch((error) => {
      reportControlError('Could not acknowledge the pending Apply launch', error);
    });
  }, [reportControlError, requestRender, resetPowerActions, resetRunControls, runStateRef]);

  return {
    armLaunch,
    pendingLaunchRef,
    runRejectionMessage,
    setRunRejectionMessage,
  };
}
