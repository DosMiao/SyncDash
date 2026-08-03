import { useCallback, useRef, useState } from 'react';
import type { MutableRefObject } from 'react';
import type { RunState } from '#core/application/progress/runstate.ts';
import { progressPlatformActions } from './progressPlatformActions.ts';
import type { PausePending, StopState } from '../model/progressPresentation.ts';
import type { PauseRequest, StopRequest } from './progressRuntimeTypes.ts';

interface ProgressRunControlsOptions {
  runStateRef: MutableRefObject<RunState>;
  requestRender: () => void;
  reportControlError: (action: string, error: unknown) => void;
}

/** Owns optimistic pause/resume and cooperative stop request fences. */
export function useProgressRunControls({
  runStateRef,
  requestRender,
  reportControlError,
}: ProgressRunControlsOptions) {
  const pauseRequestRef = useRef<PauseRequest | null>(null);
  const [pausePending, setPausePending] = useState<PausePending>(null);
  const stopRequestRef = useRef<StopRequest | null>(null);
  const [stopState, setStopState] = useState<StopState>('idle');

  const resetRequests = useCallback(() => {
    pauseRequestRef.current = null;
    stopRequestRef.current = null;
    setPausePending(null);
    setStopState('idle');
  }, []);

  const togglePause = useCallback(async () => {
    const currentRunState = runStateRef.current;
    if (pauseRequestRef.current !== null
      || stopRequestRef.current !== null
      || stopState !== 'idle'
      || !currentRunState.running
      || currentRunState.summary
      || currentRunState.runId < 0
    ) return;

    const request: PauseRequest = {
      runId: currentRunState.runId,
      pause: currentRunState.pausedSince === 0,
    };
    const previousPausedSince = currentRunState.pausedSince;
    pauseRequestRef.current = request;
    setPausePending(request.pause ? 'pause' : 'resume');
    currentRunState.pausedSince = request.pause ? Date.now() : 0;
    requestRender();

    try {
      const accepted = await progressPlatformActions.setPaused(request.runId, request.pause);
      if (!accepted) throw new Error('the run already finished');
    } catch (error) {
      if (pauseRequestRef.current === request && runStateRef.current.runId === request.runId) {
        runStateRef.current.pausedSince = previousPausedSince;
        reportControlError(request.pause ? 'Could not pause this run' : 'Could not resume this run', error);
        requestRender();
      }
    } finally {
      if (pauseRequestRef.current === request) {
        pauseRequestRef.current = null;
        setPausePending(null);
      }
    }
  }, [reportControlError, requestRender, runStateRef, stopState]);

  const stopRun = useCallback(async () => {
    const currentRunState = runStateRef.current;
    if (stopRequestRef.current !== null
      || !currentRunState.running
      || currentRunState.summary
      || currentRunState.runId < 0
    ) return;

    const request: StopRequest = { runId: currentRunState.runId };
    const previousPausedSince = currentRunState.pausedSince;
    stopRequestRef.current = request;
    pauseRequestRef.current = null;
    setPausePending(null);
    setStopState('stopping');
    let resumeAccepted = previousPausedSince === 0;
    let failureAction = 'Could not stop this run';

    try {
      if (previousPausedSince !== 0) {
        failureAction = 'Could not resume before stopping';
        currentRunState.pausedSince = 0;
        requestRender();
        const resumed = await progressPlatformActions.setPaused(request.runId, false);
        if (stopRequestRef.current !== request || runStateRef.current.runId !== request.runId) return;
        if (!resumed) {
          setStopState('finished');
          return;
        }
        resumeAccepted = true;
      }

      failureAction = 'Could not stop this run';
      const cancellationRequested = await progressPlatformActions.cancelRun(request.runId);
      if (stopRequestRef.current !== request || runStateRef.current.runId !== request.runId) return;
      if (!cancellationRequested) setStopState('finished');
    } catch (error) {
      if (stopRequestRef.current === request && runStateRef.current.runId === request.runId) {
        if (!resumeAccepted && previousPausedSince !== 0) {
          runStateRef.current.pausedSince = previousPausedSince;
        }
        setStopState('idle');
        reportControlError(failureAction, error);
        requestRender();
      }
    } finally {
      if (stopRequestRef.current === request) stopRequestRef.current = null;
    }
  }, [reportControlError, requestRender, runStateRef]);

  return {
    pausePending,
    pauseRequestRef,
    resetRequests,
    setPausePending,
    setStopState,
    stopRequestRef,
    stopRun,
    stopState,
    togglePause,
  };
}
