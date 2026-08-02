import { useCallback, useEffect } from 'react';
import type { Dispatch, MutableRefObject, SetStateAction } from 'react';
import { mergeRunEventReplay } from '#core/domain/runs/runEvents.ts';
import { replayApplyEvents, reportProgressWindowMounted } from '#core/infrastructure/tauri/commands/progress.ts';
import { listenToProgressWindowEvent as listen } from '#core/infrastructure/tauri/progressWindow.ts';
import type { PostRunPowerActionReadyDto } from '#core/types/generated/PostRunPowerActionReadyDto.ts';
import { deriveAutoCloseRequest, derivePowerActionCountdown } from '#core/application/progress/postRunActions.ts';
import type {
  AutoCloseRequest,
  PowerActionCountdown,
  WhenFinishedAction,
} from '#core/application/progress/postRunActions.ts';
import type { ProgressRateSample, RunProgressEvent, RunState } from '#core/application/progress/runstate.ts';
import { activeElapsedMs, endStage, newRunState, startStage } from '#core/application/progress/runstate.ts';
import { formatStageProgress, type PowerActionFailure } from '../../model/progressPresentation.ts';
import type { PauseRequest, PendingLaunch, StopRequest, StopState } from '../progressRuntimeTypes.ts';

interface RunRejectionEvent {
  launch_id: number;
  message: string;
}

interface ProgressRunEventsOptions {
  runStateRef: MutableRefObject<RunState>;
  pendingLaunchRef: MutableRefObject<PendingLaunch | null>;
  pauseRequestRef: MutableRefObject<PauseRequest | null>;
  stopRequestRef: MutableRefObject<StopRequest | null>;
  powerActionReadyRunIdRef: MutableRefObject<number | null>;
  autoCloseEnabledRef: MutableRefObject<boolean>;
  whenFinishedActionRef: MutableRefObject<WhenFinishedAction>;
  setRunRejectionMessage: Dispatch<SetStateAction<string | null>>;
  setPausePending: Dispatch<SetStateAction<'pause' | 'resume' | null>>;
  setStopState: Dispatch<SetStateAction<StopState>>;
  setScheduledAutoClose: Dispatch<SetStateAction<AutoCloseRequest | null>>;
  setPowerActionCountdown: Dispatch<SetStateAction<PowerActionCountdown | null>>;
  setPowerActionFailure: Dispatch<SetStateAction<PowerActionFailure | null>>;
  armLaunch: (launchId: number) => void;
  resetRunControlRequests: () => void;
  requestRender: () => void;
  requestWindowDestruction: () => Promise<void>;
  reportControlError: (action: string, error: unknown) => void;
}

const initialProgressLaunchId = (() => {
  const launchIdParameter = new URLSearchParams(window.location.search).get('launch_id');
  if (launchIdParameter === null) return null;
  const launchId = Number(launchIdParameter);
  return Number.isSafeInteger(launchId) && launchId > 0 ? launchId : null;
})();

export function useProgressRunEvents({
  runStateRef,
  pendingLaunchRef,
  pauseRequestRef,
  stopRequestRef,
  powerActionReadyRunIdRef,
  autoCloseEnabledRef,
  whenFinishedActionRef,
  setRunRejectionMessage,
  setPausePending,
  setStopState,
  setScheduledAutoClose,
  setPowerActionCountdown,
  setPowerActionFailure,
  armLaunch,
  resetRunControlRequests,
  requestRender,
  requestWindowDestruction,
  reportControlError,
}: ProgressRunEventsOptions) {
  const handleRunProgressEvent = useCallback((event: RunProgressEvent) => {
    const previousRunState = runStateRef.current;
    if (event.purpose === 'compare') return;
    const pendingLaunch = pendingLaunchRef.current;
    if (pendingLaunch && event.run_id <= pendingLaunch.afterRunId) return;
    if (event.run_id < previousRunState.runId) return;
    if (event.run_id > previousRunState.runId) {
      const closeAfterStop = previousRunState.closeAfterStop;
      runStateRef.current = newRunState(event.run_id, event.ts_ms);
      runStateRef.current.closeAfterStop = closeAfterStop;
      if (powerActionReadyRunIdRef.current !== null
        && powerActionReadyRunIdRef.current < event.run_id) {
        powerActionReadyRunIdRef.current = null;
      }
      setRunRejectionMessage(null);
      resetRunControlRequests();
      setScheduledAutoClose(null);
      setPowerActionCountdown(null);
      setPowerActionFailure(null);
    }
    const currentRunState = runStateRef.current;

    switch (event.kind) {
      case 'phase_start': {
        const phase = event.phase!;
        currentRunState.phase = phase;
        let stage = currentRunState.stages.find((candidate) => candidate.phase === phase);
        if (!stage) {
          stage = { phase, detail: '', active: true, done: false };
          currentRunState.stages.push(stage);
        }
        startStage(stage);
        if (event.label) stage.detail = event.label;
        currentRunState.applying = true;
        // Each Apply phase owns a counter epoch; refresh and ship totals cannot share its meter.
        currentRunState.totals = { items: event.items_total ?? 0, bytes: event.bytes_total ?? 0 };
        currentRunState.completed = { items: 0, bytes: 0 };
        currentRunState.samples = [{
          activeElapsedMs: activeElapsedMs(currentRunState),
          bytesDone: 0,
          itemsDone: 0,
        }];
        currentRunState.currentPath = '';
        break;
      }
      case 'totals':
        if (event.phase === currentRunState.phase) {
          currentRunState.totals = {
            items: event.items_total ?? 0,
            bytes: event.bytes_total ?? 0,
          };
          currentRunState.completed = event.reset
            ? { items: event.items_done ?? 0, bytes: event.bytes_done ?? 0 }
            : {
                items: Math.max(currentRunState.completed.items, event.items_done ?? 0),
                bytes: Math.max(currentRunState.completed.bytes, event.bytes_done ?? 0),
              };
          const sample: ProgressRateSample = {
            activeElapsedMs: activeElapsedMs(currentRunState),
            bytesDone: currentRunState.completed.bytes,
            itemsDone: currentRunState.completed.items,
          };
          if (event.reset) currentRunState.samples = [sample];
          else currentRunState.samples.push(sample);
        }
        break;
      case 'progress': {
        let stage = currentRunState.stages.find((candidate) => candidate.phase === event.phase);
        if (!stage) {
          stage = { phase: event.phase!, detail: '', active: true, done: false };
          currentRunState.stages.push(stage);
        }
        stage.detail = formatStageProgress(event);
        if (event.phase === currentRunState.phase) {
          // Worker events can reach the webview in a different order. Totals is the explicit epoch
          // reset (walk → hash); inside an epoch, counters never regress.
          currentRunState.completed = {
            items: Math.max(currentRunState.completed.items, event.items_done ?? 0),
            bytes: Math.max(currentRunState.completed.bytes, event.bytes_done ?? 0),
          };
          currentRunState.totals = {
            items: event.items_total ?? currentRunState.totals.items,
            bytes: event.bytes_total ?? currentRunState.totals.bytes,
          };
          currentRunState.currentPath = event.current_path ?? currentRunState.currentPath;
          currentRunState.samples.push({
            activeElapsedMs: activeElapsedMs(currentRunState),
            bytesDone: currentRunState.completed.bytes,
            itemsDone: currentRunState.completed.items,
          });
          if (currentRunState.samples.length > 4000) currentRunState.samples.splice(0, 1000);
        }
        break;
      }
      case 'phase_end': {
        const stage = currentRunState.stages.find((candidate) => candidate.phase === event.phase);
        if (stage) {
          endStage(stage, event.status);
          stage.detail = formatStageProgress(event);
        }
        if (event.phase === currentRunState.phase) {
          currentRunState.completed = {
            items: event.items_done ?? currentRunState.completed.items,
            bytes: event.bytes_done ?? currentRunState.completed.bytes,
          };
          currentRunState.totals = {
            items: event.items_total ?? currentRunState.totals.items,
            bytes: event.bytes_total ?? currentRunState.totals.bytes,
          };
          currentRunState.samples.push({
            activeElapsedMs: activeElapsedMs(currentRunState),
            bytesDone: currentRunState.completed.bytes,
            itemsDone: currentRunState.completed.items,
          });
        }
        break;
      }
      case 'error':
        currentRunState.errors.push({
          path: event.path ?? '', action: event.action ?? '', side: event.side ?? '',
          message: event.message ?? '', warning: event.action === 'warning',
        });
        requestRender();
        break;
      case 'log':
        if (event.level === 'warn' || event.level === 'error') {
          currentRunState.errors.push({
            path: '', action: event.scope ?? 'log', side: '',
            message: event.message ?? '', warning: event.level === 'warn',
          });
          requestRender();
        }
        break;
      case 'paused':
        currentRunState.pausedSince = Date.now();
        requestRender();
        break;
      case 'resumed':
        currentRunState.pausedSince = 0;
        currentRunState.pausedMs = event.paused_ms ?? currentRunState.pausedMs;
        requestRender();
        break;
      case 'summary': {
        pendingLaunchRef.current = null;
        pauseRequestRef.current = null;
        stopRequestRef.current = null;
        setPausePending(null);
        setStopState('finished');
        setScheduledAutoClose(null);
        currentRunState.summary = event;
        currentRunState.running = false;
        currentRunState.pausedSince = 0;
        currentRunState.pausedMs = event.paused_ms ?? currentRunState.pausedMs;
        // Throttling can omit the final progress event before a successful summary arrives.
        if (!event.cancelled
          && (event.errors ?? 0) === 0
          && currentRunState.totals.bytes + currentRunState.totals.items > 0
        ) {
          currentRunState.completed = {
            items: currentRunState.totals.items,
            bytes: currentRunState.totals.bytes,
          };
          currentRunState.samples.push({
            activeElapsedMs: activeElapsedMs(currentRunState),
            bytesDone: currentRunState.completed.bytes,
            itemsDone: currentRunState.completed.items,
          });
        }
        for (const stage of currentRunState.stages) stage.active = false;
        const finalStage = currentRunState.stages.find(
          (candidate) => candidate.phase === currentRunState.phase,
        );
        if (finalStage && !finalStage.done && !finalStage.failed && !finalStage.cancelled) {
          finalStage.cancelled = !!event.cancelled;
          finalStage.failed = !event.cancelled;
        }
        requestRender();
        if (currentRunState.closeAfterStop) {
          void requestWindowDestruction();
          break;
        }
        if (event.cancelled) break;

        const autoCloseRequest = deriveAutoCloseRequest({
          completedRunId: event.run_id,
          currentRunId: currentRunState.runId,
          summary: event,
          applying: currentRunState.applying,
          autoCloseEnabled: autoCloseEnabledRef.current,
          closeAfterStop: currentRunState.closeAfterStop,
        });
        if (autoCloseRequest) {
          setScheduledAutoClose(autoCloseRequest);
          break;
        }

        const nextPowerActionCountdown = derivePowerActionCountdown({
          readyRunId: powerActionReadyRunIdRef.current,
          currentRunId: currentRunState.runId,
          summary: event,
          applying: currentRunState.applying,
          autoCloseEnabled: autoCloseEnabledRef.current,
          whenFinishedAction: whenFinishedActionRef.current,
        });
        if (nextPowerActionCountdown) setPowerActionCountdown(nextPowerActionCountdown);
        break;
      }
    }
  }, [
    autoCloseEnabledRef,
    pauseRequestRef,
    pendingLaunchRef,
    powerActionReadyRunIdRef,
    requestRender,
    requestWindowDestruction,
    resetRunControlRequests,
    runStateRef,
    setPausePending,
    setPowerActionCountdown,
    setPowerActionFailure,
    setRunRejectionMessage,
    setScheduledAutoClose,
    setStopState,
    stopRequestRef,
    whenFinishedActionRef,
  ]);

  useEffect(() => {
    let disposed = false;
    let stopListening: (() => void) | undefined;
    let replayMerged = false;
    let lastSequence = 0;
    const queuedEvents: RunProgressEvent[] = [];
    const publishOrderedEvent = (event: RunProgressEvent) => {
      if (!replayMerged) {
        queuedEvents.push(event);
        return;
      }
      if (event.sequence <= lastSequence) return;
      lastSequence = event.sequence;
      handleRunProgressEvent(event);
    };
    void Promise.allSettled([
      listen<RunProgressEvent>('run-progress', (event) => publishOrderedEvent(event.payload)),
      listen<RunRejectionEvent>('run-rejected', (event) => {
        const pendingLaunch = pendingLaunchRef.current;
        if (!pendingLaunch || event.payload.launch_id !== pendingLaunch.launchId) return;
        pendingLaunchRef.current = null;
        powerActionReadyRunIdRef.current = null;
        setScheduledAutoClose(null);
        const closeAfterStop = runStateRef.current.closeAfterStop;
        const resetRunState = newRunState();
        resetRunState.runId = pendingLaunch.afterRunId;
        runStateRef.current = resetRunState;
        setRunRejectionMessage(event.payload.message);
        resetRunControlRequests();
        setStopState('finished');
        requestRender();
        if (closeAfterStop) void requestWindowDestruction();
      }),
      listen<number>('progress-window-arm', (event) => armLaunch(event.payload)),
      listen<PostRunPowerActionReadyDto>('post-run-power-action-ready', (event) => {
        const runId = event.payload.run_id;
        const currentRunState = runStateRef.current;
        if (runId < currentRunState.runId) return;
        powerActionReadyRunIdRef.current = runId;
        const nextPowerActionCountdown = derivePowerActionCountdown({
          readyRunId: runId,
          currentRunId: currentRunState.runId,
          summary: currentRunState.summary,
          applying: currentRunState.applying,
          autoCloseEnabled: autoCloseEnabledRef.current,
          whenFinishedAction: whenFinishedActionRef.current,
        });
        if (nextPowerActionCountdown) setPowerActionCountdown(nextPowerActionCountdown);
      }),
    ]).then(async (listenerResults) => {
      const stopListeningCallbacks = listenerResults.flatMap((result) => (
        result.status === 'fulfilled' ? [result.value] : []
      ));
      const listenerFailures = listenerResults.flatMap((result) => (
        result.status === 'rejected' ? [String(result.reason)] : []
      ));
      if (listenerFailures.length > 0) {
        for (const stopListener of stopListeningCallbacks) stopListener();
        if (!disposed) {
          reportControlError('Could not attach all progress event listeners', listenerFailures.join(' · '));
        }
        return;
      }
      if (disposed) {
        for (const stopListener of stopListeningCallbacks) stopListener();
        return;
      }
      stopListening = () => {
        for (const stopListener of stopListeningCallbacks) stopListener();
      };
      try {
        const replayedEvents = await replayApplyEvents();
        if (!disposed) {
          const pendingEvents = mergeRunEventReplay(replayedEvents, queuedEvents);
          queuedEvents.length = 0;
          replayMerged = true;
          for (const event of pendingEvents) publishOrderedEvent(event);
        }
      } catch (error) {
        if (disposed) return;
        replayMerged = true;
        for (const event of queuedEvents.splice(0)) publishOrderedEvent(event);
        reportControlError('Could not restore progress after reconnect', error);
      }
      if (disposed) return;
      if (initialProgressLaunchId === null) {
        reportControlError('Could not announce the mounted progress window', 'missing or invalid launch identity');
        return;
      }
      try { await reportProgressWindowMounted(initialProgressLaunchId); }
      catch (error) {
        if (!disposed) reportControlError('Could not announce the mounted progress window', error);
      }
    }).catch((error) => {
      if (!disposed) reportControlError('Progress event setup failed', error);
    });
    return () => {
      disposed = true;
      stopListening?.();
    };
  }, [
    armLaunch,
    autoCloseEnabledRef,
    handleRunProgressEvent,
    pendingLaunchRef,
    powerActionReadyRunIdRef,
    reportControlError,
    requestRender,
    requestWindowDestruction,
    resetRunControlRequests,
    runStateRef,
    setPowerActionCountdown,
    setRunRejectionMessage,
    setScheduledAutoClose,
    setStopState,
    whenFinishedActionRef,
  ]);
}
