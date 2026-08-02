import { useEffect } from 'react';
import type { Dispatch, MutableRefObject, SetStateAction } from 'react';
import {
  beginProgressWindowClose,
  cancelApplyRun,
  setApplyPaused,
} from '#core/infrastructure/tauri/commands/progress.ts';
import {
  getProgressWindow,
  ProgressBarStatus,
} from '#core/infrastructure/tauri/progressWindow.ts';
import { applyZoom } from '#core/infrastructure/tauri/webviewZoom.ts';
import type { ZoomPreferenceLoad } from '#core/application/zoom/zoomAuthority.ts';
import type { RunState } from '#core/application/progress/runstate.ts';
import { PHASE_LABELS } from '#core/application/progress/runstate.ts';
import type {
  AutoCloseRequest,
  PowerActionCountdown,
} from '#core/application/progress/postRunActions.ts';
import type { PauseRequest, PendingLaunch, StopRequest, StopState } from '../progressRuntimeTypes.ts';

const progressWindow = getProgressWindow();

type ErrorReporter = (action: string, error: unknown) => void;

export function useProgressWindowZoom(
  zoomPreference: ZoomPreferenceLoad,
  reportWindowChromeFailure: ErrorReporter,
) {
  useEffect(() => {
    if (zoomPreference.warning) {
      reportWindowChromeFailure('Could not restore the progress-window zoom preference', zoomPreference.warning);
    }
    void applyZoom(zoomPreference.factor).catch((error) => {
      reportWindowChromeFailure('Could not restore the progress-window zoom', error);
    });
  }, [reportWindowChromeFailure, zoomPreference]);
}

interface ProgressWindowChromeOptions {
  runState: RunState;
  completionPercentage: number;
  runPaused: boolean;
  runRejectionMessage: string | null;
  reportWindowChromeFailure: ErrorReporter;
}

export function useProgressWindowChrome({
  runState,
  completionPercentage,
  runPaused,
  runRejectionMessage,
  reportWindowChromeFailure,
}: ProgressWindowChromeOptions) {
  useEffect(() => {
    const title = runState.summary
      ? (runState.summary.cancelled ? 'Stopped — SyncDash' : 'Done — SyncDash')
      : runRejectionMessage ? 'Could not start — SyncDash'
      : runState.applying
        ? `${Math.round(completionPercentage)}% — SyncDash`
        : `${runState.phase ? PHASE_LABELS[runState.phase] : 'Running'} — SyncDash`;
    void progressWindow.setTitle(title).catch((error) => {
      reportWindowChromeFailure('Could not update the progress window title', error);
    });
    if (runState.summary) {
      void progressWindow.setProgressBar({ status: ProgressBarStatus.None }).catch((error) => {
        reportWindowChromeFailure('Could not clear operating-system progress', error);
      });
    } else if (runState.applying) {
      void progressWindow.setProgressBar({
        status: runPaused ? ProgressBarStatus.Paused : ProgressBarStatus.Normal,
        progress: Math.round(completionPercentage),
      }).catch((error) => {
        reportWindowChromeFailure('Could not update operating-system progress', error);
      });
    }
  }, [
    completionPercentage,
    reportWindowChromeFailure,
    runPaused,
    runRejectionMessage,
    runState.applying,
    runState.phase,
    runState.summary,
  ]);
}

interface ProgressWindowCloseOptions {
  runStateRef: MutableRefObject<RunState>;
  pendingLaunchRef: MutableRefObject<PendingLaunch | null>;
  pauseRequestRef: MutableRefObject<PauseRequest | null>;
  stopRequestRef: MutableRefObject<StopRequest | null>;
  closeRequestPendingRef: MutableRefObject<boolean>;
  windowDestructionPendingRef: MutableRefObject<boolean>;
  setScheduledAutoClose: Dispatch<SetStateAction<AutoCloseRequest | null>>;
  setPowerActionCountdown: Dispatch<SetStateAction<PowerActionCountdown | null>>;
  setPausePending: Dispatch<SetStateAction<'pause' | 'resume' | null>>;
  setStopState: Dispatch<SetStateAction<StopState>>;
  requestRender: () => void;
  requestWindowDestruction: () => Promise<void>;
  reportControlError: ErrorReporter;
  reportWindowChromeFailure: ErrorReporter;
}

export function useProgressWindowClose({
  runStateRef,
  pendingLaunchRef,
  pauseRequestRef,
  stopRequestRef,
  closeRequestPendingRef,
  windowDestructionPendingRef,
  setScheduledAutoClose,
  setPowerActionCountdown,
  setPausePending,
  setStopState,
  requestRender,
  requestWindowDestruction,
  reportControlError,
  reportWindowChromeFailure,
}: ProgressWindowCloseOptions) {
  useEffect(() => {
    let disposed = false;
    let removeCloseListener: (() => void) | null = null;
    void progressWindow.onCloseRequested(async (event) => {
      event.preventDefault();
      if (closeRequestPendingRef.current || windowDestructionPendingRef.current) return;
      closeRequestPendingRef.current = true;
      setScheduledAutoClose(null);
      setPowerActionCountdown(null);
      pauseRequestRef.current = null;
      stopRequestRef.current = null;
      setPausePending(null);
      try {
        // React may still describe the previous run while a launch is being armed. The backend owns
        // the atomic reservation-to-Apply transition, so closing cannot start a headless sync.
        runStateRef.current.closeAfterStop = true;
        let closeDecision: Awaited<ReturnType<typeof beginProgressWindowClose>>;
        try { closeDecision = await beginProgressWindowClose(); }
        catch (error) { reportControlError('Could not inspect the active launch before closing', error); return; }
        let latestRunState = runStateRef.current;
        latestRunState.closeAfterStop = true;
        if (closeDecision.decision === 'pending_launch_cancelled') {
          pendingLaunchRef.current = null;
          void requestWindowDestruction();
          return;
        }
        if (closeDecision.decision === 'active_run_cancellation_requested') {
          if (latestRunState.runId === closeDecision.run_id && latestRunState.summary) {
            void requestWindowDestruction();
          } else {
            setStopState('stopping');
          }
          return;
        }

        // An unattended Apply still needs cooperative cancellation before window destruction.
        if (latestRunState.running && !latestRunState.summary) {
          const runId = latestRunState.runId;
          setStopState('stopping');
          if (runId < 0) {
            setStopState('idle');
            return;
          }
          if (latestRunState.pausedSince) {
            const previousPausedSince = latestRunState.pausedSince;
            latestRunState.pausedSince = 0;
            requestRender();
            try { await setApplyPaused(runId, false); }
            catch (error) {
              if (runStateRef.current.runId === runId) {
                runStateRef.current.pausedSince = previousPausedSince;
              }
              setStopState('idle');
              reportControlError('Could not resume before stopping', error);
              return;
            }
          }
          let cancellationRequested: boolean;
          try { cancellationRequested = await cancelApplyRun(runId); }
          catch (error) {
            setStopState('idle');
            reportControlError('Could not stop this run', error);
            return;
          }
          if (!cancellationRequested) {
            // Catch a launch reserved while cancelApplyRun was in flight before destroying the window.
            let racedDecision: Awaited<ReturnType<typeof beginProgressWindowClose>>;
            try { racedDecision = await beginProgressWindowClose(); }
            catch (error) {
              setStopState('idle');
              reportControlError('Could not inspect a raced launch before closing', error);
              return;
            }
            latestRunState = runStateRef.current;
            latestRunState.closeAfterStop = true;
            if (racedDecision.decision === 'active_run_cancellation_requested') {
              if (latestRunState.runId === racedDecision.run_id && latestRunState.summary) {
                void requestWindowDestruction();
              } else {
                setStopState('stopping');
              }
              return;
            }
            if (racedDecision.decision === 'pending_launch_cancelled') {
              pendingLaunchRef.current = null;
            }
          } else {
            return;
          }
        }
        void requestWindowDestruction();
      } finally {
        closeRequestPendingRef.current = false;
      }
    }).then(
      (stopListening) => {
        if (disposed) stopListening();
        else removeCloseListener = stopListening;
      },
      (error: unknown) => {
        if (!disposed) reportWindowChromeFailure('Could not attach the progress-window close handler', error);
      },
    );
    return () => {
      disposed = true;
      removeCloseListener?.();
    };
  }, [
    closeRequestPendingRef,
    pauseRequestRef,
    pendingLaunchRef,
    reportControlError,
    reportWindowChromeFailure,
    requestRender,
    requestWindowDestruction,
    runStateRef,
    setPausePending,
    setPowerActionCountdown,
    setScheduledAutoClose,
    setStopState,
    stopRequestRef,
    windowDestructionPendingRef,
  ]);
}
