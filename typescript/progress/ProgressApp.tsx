import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { Check, Pause, Play, RefreshCw, Square, TriangleAlert } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, ProgressBarStatus } from '@tauri-apps/api/window';
import {
  acknowledgeProgressLaunch,
  beginProgressWindowClose,
  cancelApplyRun,
  destroyProgressWindow,
  executePostRunPowerAction,
  replayApplyEvents,
  reportProgressWindowMounted,
  setApplyPaused,
} from '../core/ipc';
import { humanDuration, humanSize } from '../core/format';
import { mergeRunEventReplay } from '../core/runEvents';
import { applyZoom, loadZoomPreference } from '../core/zoom';
import type { PostRunPowerActionReadyDto } from '../core/types/generated/PostRunPowerActionReadyDto';
import { Graph } from './Graph';
import {
  isWhenFinishedAction,
  loadProgressPreferences,
  saveAutoClosePreference,
  saveWhenFinishedPreference,
} from './preferences';
import type { WhenFinishedAction } from './preferences';
import { deriveAutoCloseRequest, derivePowerActionCountdown } from './postRunActions';
import type { AutoCloseRequest, PowerAction, PowerActionCountdown } from './postRunActions';
import type { ProgressRateSample, RunProgressEvent, RunState } from './runstate';
import {
  PHASE_LABELS,
  activeElapsedMs,
  calculateWindowRate,
  completionPercent,
  endStage,
  newRunState,
  startStage,
} from './runstate';

const progressWindow = getCurrentWindow();
const initialProgressLaunchId = (() => {
  const launchIdParameter = new URLSearchParams(window.location.search).get('launch_id');
  if (launchIdParameter === null) return null;
  const launchId = Number(launchIdParameter);
  return Number.isSafeInteger(launchId) && launchId > 0 ? launchId : null;
})();

interface PendingLaunch { launchId: number; afterRunId: number }
interface RunRejectionEvent { launch_id: number; message: string }
interface PauseRequest { runId: number; pause: boolean }
interface StopRequest { runId: number }
interface PowerActionRequest { runId: number; action: PowerAction }

function formatStageProgress(event: RunProgressEvent): string {
  const itemTotal = event.items_total ?? 0;
  const byteTotal = event.bytes_total ?? 0;
  const completedItems = event.items_done ?? 0;
  const completedBytes = humanSize(event.bytes_done ?? 0);
  const itemProgress = itemTotal ? `${completedItems} / ${itemTotal}` : `${completedItems}`;
  const byteProgress = byteTotal ? `${completedBytes} / ${humanSize(byteTotal)}` : completedBytes;
  return `${itemProgress} items · ${byteProgress}`;
}

function formatCompletedRunStatus(summary: NonNullable<RunState['summary']>): string {
  if (summary.cancelled) {
    return `Cancelled — ${summary.done} applied, ${summary.skipped} skipped`;
  }
  return [
    `Done — ${summary.done} applied, ${summary.skipped} skipped, ${summary.errors} errors`,
    humanSize(summary.bytes_done ?? 0),
    humanDuration(summary.elapsed_ms ?? 0),
  ].join(' · ');
}

export function ProgressApp() {
  const runStateRef = useRef<RunState>(newRunState());
  const pendingLaunchRef = useRef<PendingLaunch | null>(null);
  const reportedWindowChromeFailuresRef = useRef(new Set<string>());
  const [, setRenderRevision] = useState(0);
  const requestRender = useCallback(() => setRenderRevision((revision) => revision + 1), []);

  const [storedProgressPreferences] = useState(() => loadProgressPreferences(localStorage));
  const [zoomPreference] = useState(() => loadZoomPreference(localStorage));
  const [autoCloseEnabled, setAutoCloseEnabled] = useState(storedProgressPreferences.autoCloseEnabled);
  const [whenFinishedAction, setWhenFinishedAction] = useState<WhenFinishedAction>(
    storedProgressPreferences.whenFinishedAction,
  );
  const [powerActionCountdown, setPowerActionCountdown] = useState<PowerActionCountdown | null>(null);
  const [scheduledAutoClose, setScheduledAutoClose] = useState<AutoCloseRequest | null>(null);
  const [powerActionFailure, setPowerActionFailure] = useState<{
    action: PowerAction;
    runId: number;
    error: string;
  } | null>(null);
  const powerActionRequestRef = useRef<PowerActionRequest | null>(null);
  const [powerActionPending, setPowerActionPending] = useState<PowerAction | null>(null);
  const powerActionReadyRunIdRef = useRef<number | null>(null);
  const [errorDetailsOpen, setErrorDetailsOpen] = useState(false);
  const [runRejectionMessage, setRunRejectionMessage] = useState<string | null>(null);
  const countdownProgressFillRef = useRef<HTMLDivElement>(null);
  const countdownCancelButtonRef = useRef<HTMLButtonElement>(null);
  const countdownTitleId = useId();
  const countdownDescriptionId = useId();
  const pauseRequestRef = useRef<PauseRequest | null>(null);
  const [pausePending, setPausePending] = useState<'pause' | 'resume' | null>(null);
  const stopRequestRef = useRef<StopRequest | null>(null);
  const closeRequestPendingRef = useRef(false);
  const windowDestructionPendingRef = useRef(false);
  const [stopState, setStopState] = useState<'idle' | 'stopping' | 'finished'>('idle');
  const autoCloseEnabledRef = useRef(autoCloseEnabled);
  autoCloseEnabledRef.current = autoCloseEnabled;
  const whenFinishedActionRef = useRef(whenFinishedAction);
  whenFinishedActionRef.current = whenFinishedAction;

  const reportControlError = useCallback((action: string, error: unknown) => {
    runStateRef.current.errors.push({
      path: '', action: 'control', side: '', message: `${action}: ${String(error)}`, warning: false,
    });
    setErrorDetailsOpen(true);
    requestRender();
  }, [requestRender]);

  const reportWindowChromeFailure = useCallback((action: string, error: unknown) => {
    if (reportedWindowChromeFailuresRef.current.has(action)) return;
    reportedWindowChromeFailuresRef.current.add(action);
    runStateRef.current.errors.push({
      path: '', action: 'window', side: '', message: `${action}: ${String(error)}`, warning: true,
    });
    requestRender();
  }, [requestRender]);

  useEffect(() => {
    for (const failure of storedProgressPreferences.failures) {
      reportControlError('Could not restore progress preferences', failure);
    }
  }, [reportControlError, storedProgressPreferences]);

  const requestWindowDestruction = useCallback(async () => {
    if (windowDestructionPendingRef.current) return;
    setScheduledAutoClose(null);
    setPowerActionCountdown(null);
    windowDestructionPendingRef.current = true;
    try {
      await destroyProgressWindow();
    } catch (error) {
      windowDestructionPendingRef.current = false;
      reportControlError('Could not close the progress window', error);
    }
  }, [reportControlError]);

  const executePowerAction = useCallback(async (runId: number, action: PowerAction) => {
    const currentRunState = runStateRef.current;
    if (powerActionRequestRef.current !== null
      || currentRunState.runId !== runId
      || currentRunState.running
      || !currentRunState.summary
      || powerActionReadyRunIdRef.current !== runId
      || autoCloseEnabledRef.current
      || whenFinishedActionRef.current !== action
    ) return;
    const request: PowerActionRequest = { runId, action };
    powerActionRequestRef.current = request;
    setPowerActionPending(action);
    setPowerActionFailure(null);
    try {
      await executePostRunPowerAction(runId, action);
    } catch (error) {
      if (powerActionRequestRef.current === request
        && runStateRef.current.runId === runId
        && powerActionReadyRunIdRef.current === runId
        && !autoCloseEnabledRef.current
        && whenFinishedActionRef.current === action
      ) {
        setPowerActionFailure({ action, runId, error: String(error) });
        const actionDescription = action === 'sleep'
          ? 'put the computer to sleep'
          : 'shut down the computer';
        reportControlError(`Could not ${actionDescription}`, error);
      }
    } finally {
      if (powerActionRequestRef.current === request) {
        powerActionRequestRef.current = null;
        setPowerActionPending(null);
      }
    }
  }, [reportControlError]);

  const reconcilePowerActionCountdown = useCallback((
    nextWhenFinishedAction: WhenFinishedAction,
    nextAutoCloseEnabled: boolean,
  ) => {
    const currentRunState = runStateRef.current;
    setPowerActionCountdown(derivePowerActionCountdown({
      readyRunId: powerActionReadyRunIdRef.current,
      currentRunId: currentRunState.runId,
      summary: currentRunState.summary,
      applying: currentRunState.applying,
      autoCloseEnabled: nextAutoCloseEnabled,
      whenFinishedAction: nextWhenFinishedAction,
    }));
  }, []);

  const resetRunControlRequests = useCallback(() => {
    pauseRequestRef.current = null;
    stopRequestRef.current = null;
    setPausePending(null);
    setStopState('idle');
  }, []);

  const armLaunch = useCallback((launchId: number) => {
    const afterRunId = runStateRef.current.runId;
    pendingLaunchRef.current = { launchId, afterRunId };
    const resetRunState = newRunState();
    resetRunState.runId = afterRunId;
    runStateRef.current = resetRunState;
    setRunRejectionMessage(null);
    resetRunControlRequests();
    setScheduledAutoClose(null);
    setPowerActionCountdown(null);
    setPowerActionFailure(null);
    powerActionReadyRunIdRef.current = null;
    requestRender();
    void acknowledgeProgressLaunch(launchId).catch((error) => {
      reportControlError('Could not acknowledge the pending Apply launch', error);
    });
  }, [reportControlError, requestRender, resetRunControlRequests]);

  useEffect(() => {
    if (zoomPreference.warning) {
      reportWindowChromeFailure('Could not restore the progress-window zoom preference', zoomPreference.warning);
    }
    void applyZoom(zoomPreference.factor).catch((error) => {
      reportWindowChromeFailure('Could not restore the progress-window zoom', error);
    });
  }, [reportWindowChromeFailure, zoomPreference]);

  const formatByteRate = useCallback((runState: RunState) => {
    const rate = calculateWindowRate(runState, 4000);
    return rate ? `${(rate.bytesPerSecond / (1 << 20)).toFixed(2)} MiB/s` : '';
  }, []);
  const formatItemRate = useCallback((runState: RunState) => {
    const rate = calculateWindowRate(runState, 4000);
    return rate ? `${rate.itemsPerSecond.toFixed(0)} items/s` : '';
  }, []);

  const cancelPowerActionCountdown = useCallback(() => setPowerActionCountdown(null), []);
  const powerActionCountdownIdentity = powerActionCountdown
    ? `${powerActionCountdown.runId}:${powerActionCountdown.action}`
    : null;

  useEffect(() => {
    if (!powerActionCountdown) return;
    if (powerActionCountdown.secondsRemaining <= 0) {
      setPowerActionCountdown(null);
      void executePowerAction(powerActionCountdown.runId, powerActionCountdown.action);
      return;
    }
    const countdownTimerId = window.setTimeout(() => {
      setPowerActionCountdown((currentCountdown) => (
        currentCountdown
        && currentCountdown.runId === powerActionCountdown.runId
        && currentCountdown.action === powerActionCountdown.action
          ? { ...currentCountdown, secondsRemaining: currentCountdown.secondsRemaining - 1 }
          : currentCountdown
      ));
    }, 1000);
    return () => window.clearTimeout(countdownTimerId);
  }, [executePowerAction, powerActionCountdown]);

  useLayoutEffect(() => {
    if (powerActionCountdownIdentity === null) return;
    countdownCancelButtonRef.current?.focus();
  }, [powerActionCountdownIdentity]);

  useEffect(() => {
    if (powerActionCountdownIdentity === null) return;
    const handleCountdownKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      cancelPowerActionCountdown();
    };
    window.addEventListener('keydown', handleCountdownKeyDown);
    return () => window.removeEventListener('keydown', handleCountdownKeyDown);
  }, [cancelPowerActionCountdown, powerActionCountdownIdentity]);

  useLayoutEffect(() => {
    const progressFill = countdownProgressFillRef.current;
    if (!progressFill || !powerActionCountdown) return;
    progressFill.style.setProperty(
      '--countdown-progress-width',
      `${powerActionCountdown.secondsRemaining * 10}%`,
    );
    return () => { progressFill.style.removeProperty('--countdown-progress-width'); };
  }, [powerActionCountdown]);

  useEffect(() => {
    if (!scheduledAutoClose) return;
    const autoCloseTimerId = window.setTimeout(() => {
      const currentRunState = runStateRef.current;
      const validatedAutoCloseRequest = deriveAutoCloseRequest({
        completedRunId: scheduledAutoClose.runId,
        currentRunId: currentRunState.runId,
        summary: currentRunState.summary,
        applying: currentRunState.applying,
        autoCloseEnabled: autoCloseEnabledRef.current,
        closeAfterStop: currentRunState.closeAfterStop,
      });
      if (!validatedAutoCloseRequest) {
        setScheduledAutoClose((request) => (request === scheduledAutoClose ? null : request));
        return;
      }
      setScheduledAutoClose((request) => (request === scheduledAutoClose ? null : request));
      void requestWindowDestruction();
    }, 1200);
    return () => window.clearTimeout(autoCloseTimerId);
  }, [requestWindowDestruction, scheduledAutoClose]);

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
  }, [requestRender, requestWindowDestruction, resetRunControlRequests]);

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
    handleRunProgressEvent,
    reportControlError,
    requestRender,
    requestWindowDestruction,
    resetRunControlRequests,
  ]);

  // Numeric readouts update less often than graphs so rapidly changing figures remain readable.
  useEffect(() => {
    const renderIntervalId = window.setInterval(() => {
      if (runStateRef.current.runId >= 0) requestRender();
    }, 500);
    return () => window.clearInterval(renderIntervalId);
  }, [requestRender]);

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
      const accepted = await setApplyPaused(request.runId, request.pause);
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
  }, [reportControlError, requestRender, stopState]);

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
        const resumed = await setApplyPaused(request.runId, false);
        if (stopRequestRef.current !== request || runStateRef.current.runId !== request.runId) return;
        if (!resumed) {
          setStopState('finished');
          return;
        }
        resumeAccepted = true;
      }

      failureAction = 'Could not stop this run';
      const cancellationRequested = await cancelApplyRun(request.runId);
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
  }, [reportControlError, requestRender]);

  const currentRunState = runStateRef.current;
  const completionPercentage = completionPercent(currentRunState);
  const runPaused = currentRunState.pausedSince > 0;
  useEffect(() => {
    const title = currentRunState.summary
      ? (currentRunState.summary.cancelled ? 'Stopped — SyncDash' : 'Done — SyncDash')
      : runRejectionMessage ? 'Could not start — SyncDash'
      : currentRunState.applying
        ? `${Math.round(completionPercentage)}% — SyncDash`
        : `${currentRunState.phase ? PHASE_LABELS[currentRunState.phase] : 'Running'} — SyncDash`;
    void progressWindow.setTitle(title).catch((error) => {
      reportWindowChromeFailure('Could not update the progress window title', error);
    });
    if (currentRunState.summary) {
      void progressWindow.setProgressBar({ status: ProgressBarStatus.None }).catch((error) => {
        reportWindowChromeFailure('Could not clear operating-system progress', error);
      });
    } else if (currentRunState.applying) {
      void progressWindow.setProgressBar({
        status: runPaused ? ProgressBarStatus.Paused : ProgressBarStatus.Normal,
        progress: Math.round(completionPercentage),
      }).catch((error) => {
        reportWindowChromeFailure('Could not update operating-system progress', error);
      });
    }
  }, [
    completionPercentage,
    currentRunState.applying,
    currentRunState.phase,
    currentRunState.summary,
    reportWindowChromeFailure,
    runPaused,
    runRejectionMessage,
  ]);

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
  }, [reportControlError, reportWindowChromeFailure, requestRender, requestWindowDestruction]);

  const oneMinuteRate = calculateWindowRate(currentRunState, 60000);
  const errorCount = currentRunState.errors.filter((entry) => !entry.warning).length;
  const warningCount = currentRunState.errors.length - errorCount;

  const countersExhausted = currentRunState.totals.items + currentRunState.totals.bytes > 0
    && (currentRunState.totals.items === 0
      || currentRunState.completed.items >= currentRunState.totals.items)
    && (currentRunState.totals.bytes === 0
      || currentRunState.completed.bytes >= currentRunState.totals.bytes);
  // The copy loop reports its final byte before seal/fsync/verify/preserve/commit completes the
  // item. On an external mirror, preservation may itself be a long cross-volume copy.
  const finalizing = countersExhausted
    || (currentRunState.totals.bytes > 0
      && currentRunState.completed.bytes >= currentRunState.totals.bytes);
  const estimatedTimeRemaining = (() => {
    if (currentRunState.summary) return '—';
    if (finalizing) return 'Finalizing…';
    if (oneMinuteRate
      && currentRunState.totals.bytes > 0
      && oneMinuteRate.bytesPerSecond > 1
    ) {
      return humanDuration(
        Math.max(0, currentRunState.totals.bytes - currentRunState.completed.bytes)
          / oneMinuteRate.bytesPerSecond * 1000,
      ) + ' remaining';
    }
    if (oneMinuteRate
      && currentRunState.totals.items > 0
      && oneMinuteRate.itemsPerSecond > 0.01
    ) {
      return humanDuration(
        Math.max(0, currentRunState.totals.items - currentRunState.completed.items)
          / oneMinuteRate.itemsPerSecond * 1000,
      ) + ' remaining';
    }
    return 'Estimating…';
  })();
  const roundedCompletionPercentage = Math.round(completionPercentage);
  const progressValueText = currentRunState.summary
    ? (currentRunState.summary.cancelled
      ? `Stopped at ${roundedCompletionPercentage}%`
      : `${roundedCompletionPercentage}% complete`)
    : runRejectionMessage
      ? 'Run failed to start'
      : currentRunState.running
        ? (currentRunState.applying ? `${roundedCompletionPercentage}% complete` : 'Preparing Apply')
        : 'Waiting for a run';
  const summaryStatusText = currentRunState.summary
    ? formatCompletedRunStatus(currentRunState.summary)
    : null;

  return (
    <div className={'pwin' + (currentRunState.applying ? ' applying' : '')}>
      <div className="phead">
        <span className="pjob">SyncDash</span>
        <span className="pphase" role="status" aria-live="polite" aria-atomic="true">
          {summaryStatusText !== null
            ? summaryStatusText
            : runRejectionMessage ? `Could not start — ${runRejectionMessage}`
            : currentRunState.running
              ? <>
                  {runPaused && <><Pause size={12} /> Paused — </>}
                  {currentRunState.phase ? PHASE_LABELS[currentRunState.phase] : ''}
                  {finalizing ? ' — Finalizing' : ''}
                </>
              : 'Waiting for a run…'}
        </span>
        <span
          className={'ppct ' + (currentRunState.summary
            ? (currentRunState.summary.cancelled || currentRunState.summary.errors ? 'err' : 'ok')
            : runPaused ? 'paused' : '')}
          role="progressbar"
          aria-label="Apply progress"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={currentRunState.applying || currentRunState.summary
            ? roundedCompletionPercentage
            : undefined}
          aria-valuetext={progressValueText}
        >
          {currentRunState.summary
            ? (currentRunState.summary.cancelled ? 'Stopped' : `${roundedCompletionPercentage}%`)
            : runRejectionMessage ? 'Failed'
            : currentRunState.running
              ? (currentRunState.applying ? `${roundedCompletionPercentage}%` : '…')
              : '—'}
        </span>
      </div>

      <div className="stagerows">
        {currentRunState.stages.map((stage) => (
          <div
            key={stage.phase}
            className={'stagerow' + (stage.active ? ' active' : '') + (stage.done ? ' done' : '')}
          >
            <span className="st-ico">
              {stage.active ? <RefreshCw size={13} className="spin" />
                : stage.failed ? <TriangleAlert size={13} className="icon-err" />
                  : stage.cancelled ? <Square size={12} />
                    : stage.done ? <Check size={13} />
                      : <RefreshCw size={13} />}
            </span>
            <span className="st-name">{PHASE_LABELS[stage.phase]}</span>
            <span className="st-detail">{stage.detail}</span>
          </div>
        ))}
      </div>

      <div className="graphs">
        <Graph
          caption="Data (cumulative bytes)"
          metric="bytesDone"
          runRef={runStateRef}
          rateText={formatByteRate}
        />
        <Graph
          caption="Items (cumulative count)"
          metric="itemsDone"
          runRef={runStateRef}
          rateText={formatItemRate}
        />
        <div className="readouts">
          <div className="rh" /><div className="rh">Processed</div><div className="rh">Remaining</div>
          <div className="rh">Items</div>
          <div>{currentRunState.completed.items} / {currentRunState.totals.items}</div>
          <div>{Math.max(0, currentRunState.totals.items - currentRunState.completed.items)}</div>
          <div className="rh">Bytes</div>
          <div>{humanSize(currentRunState.completed.bytes)} / {humanSize(currentRunState.totals.bytes)}</div>
          <div>{humanSize(Math.max(0, currentRunState.totals.bytes - currentRunState.completed.bytes))}</div>
          <div className="rh">Time</div>
          <div>{humanDuration(currentRunState.summary
            ? currentRunState.summary.elapsed_ms ?? 0
            : activeElapsedMs(currentRunState))}</div>
          <div>{estimatedTimeRemaining}</div>
        </div>
        <div className="curfile" title={currentRunState.currentPath}>
          {currentRunState.currentPath ? `‎${currentRunState.currentPath}` : ''}
        </div>
      </div>

      <div className={'errsec'
        + (currentRunState.errors.length ? ' show' : '')
        + (errorDetailsOpen ? ' open' : '')}>
        <button
          type="button"
          className="errhead"
          aria-expanded={errorDetailsOpen}
          aria-controls="progress-errors"
          onClick={() => setErrorDetailsOpen((open) => !open)}
        >
          <TriangleAlert size={14} className={errorCount ? 'icon-err' : 'icon-warn'} />
          <span className="cnt-err">
            {errorCount ? `${errorCount} ${errorCount === 1 ? 'error' : 'errors'}` : ''}
          </span>
          <span className="cnt-warn">
            {warningCount ? `${warningCount} ${warningCount === 1 ? 'warning' : 'warnings'}` : ''}
          </span>
          <span className="dim errtip">{errorDetailsOpen ? 'Collapse details' : 'Expand details'}</span>
        </button>
        <div id="progress-errors" className="errlist">
          {currentRunState.errors.map((entry, index) => (
            <div key={index} className={'erow' + (entry.warning ? ' warn' : '')}>
              <span className="epath mono">{entry.path}</span>{' '}
              <span className="emsg">
                {entry.side ? `[${entry.action}/${entry.side}] ` : `[${entry.action}] `}{entry.message}
              </span>
            </div>
          ))}
        </div>
      </div>

      {powerActionCountdown && (
        <div
          className="countdown show"
          role="alertdialog"
          aria-labelledby={countdownTitleId}
          aria-describedby={countdownDescriptionId}
        >
          <span id={countdownTitleId} className="cdtext">
            <span aria-hidden="true">
              {powerActionCountdown.action === 'sleep' ? 'Sleep' : 'Shut down'} in{' '}
              {powerActionCountdown.secondsRemaining}s
            </span>
            <span className="sr-only">
              {powerActionCountdown.action === 'sleep' ? 'Computer sleep scheduled' : 'Computer shutdown scheduled'}
            </span>
          </span>
          <span id={countdownDescriptionId} className="sr-only">
            Press Escape or activate Cancel to keep the computer running.
          </span>
          <span className="sr-only" role="alert" aria-live="assertive" aria-atomic="true">
            {powerActionCountdown.action === 'sleep'
              ? 'SyncDash will put the computer to sleep in 10 seconds.'
              : 'SyncDash will shut down the computer in 10 seconds.'}
          </span>
          <div className="cdbar" aria-hidden="true">
            <div ref={countdownProgressFillRef} className="cdfill" />
          </div>
          <button
            ref={countdownCancelButtonRef}
            type="button"
            className="btn"
            autoFocus
            onClick={cancelPowerActionCountdown}
          >Cancel</button>
        </div>
      )}

      {powerActionFailure && (
        <div className="countdown show" role="alert">
          <span className="cdtext">{powerActionFailure.error}</span>
          <button
            type="button"
            className="btn"
            disabled={powerActionPending !== null
              || powerActionFailure.runId !== currentRunState.runId
              || currentRunState.running
              || !currentRunState.summary}
            onClick={() => void executePowerAction(
              powerActionFailure.runId,
              powerActionFailure.action,
            )}
          >{powerActionPending ? 'Retrying…' : 'Retry'}</button>
          <button type="button" className="btn" onClick={() => setPowerActionFailure(null)}>
            Dismiss
          </button>
        </div>
      )}

      <div className="controls">
        <button
          type="button"
          className="btn"
          disabled={!currentRunState.running
            || !!currentRunState.summary
            || pausePending !== null
            || stopState !== 'idle'}
          onClick={() => void togglePause()}
        >{pausePending === 'pause'
            ? 'Pausing…'
            : pausePending === 'resume'
              ? 'Resuming…'
              : runPaused
                ? <><Play size={12} /> Continue</>
                : <><Pause size={12} /> Pause</>}</button>
        <button
          type="button"
          className="btn btn-stop"
          disabled={!currentRunState.running || !!currentRunState.summary || stopState !== 'idle'}
          onClick={() => void stopRun()}
        >
          {stopState === 'idle'
            ? <><Square size={12} /> Stop</>
            : stopState === 'stopping' ? 'Stopping…' : 'Finished'}
        </button>
        <label className="dim chkline">
          <input
            type="checkbox"
            checked={autoCloseEnabled}
            disabled={powerActionPending !== null}
            onChange={(event) => {
              const nextAutoCloseEnabled = event.target.checked;
              const error = saveAutoClosePreference(localStorage, nextAutoCloseEnabled);
              if (error) {
                reportControlError('Could not save the Auto-close preference', error);
                return;
              }
              setScheduledAutoClose(null);
              setPowerActionFailure(null);
              autoCloseEnabledRef.current = nextAutoCloseEnabled;
              setAutoCloseEnabled(nextAutoCloseEnabled);
              reconcilePowerActionCountdown(
                whenFinishedActionRef.current,
                nextAutoCloseEnabled,
              );
            }}
          /> Auto-close when finished
        </label>
        <label className="wfin">
          When finished
          <select
            value={whenFinishedAction}
            disabled={autoCloseEnabled || powerActionPending !== null}
            title={autoCloseEnabled
              ? 'Auto-close takes precedence over a post-run power action'
              : undefined}
            onChange={(event) => {
              const nextWhenFinishedAction = event.target.value;
              if (!isWhenFinishedAction(nextWhenFinishedAction)) {
                reportControlError(
                  'Could not save the When-finished preference',
                  `unknown action: ${nextWhenFinishedAction}`,
                );
                return;
              }
              const error = saveWhenFinishedPreference(localStorage, nextWhenFinishedAction);
              if (error) {
                reportControlError('Could not save the When-finished preference', error);
                return;
              }
              setScheduledAutoClose(null);
              setPowerActionFailure(null);
              whenFinishedActionRef.current = nextWhenFinishedAction;
              setWhenFinishedAction(nextWhenFinishedAction);
              reconcilePowerActionCountdown(nextWhenFinishedAction, autoCloseEnabledRef.current);
            }}
          >
            <option value="none">Do nothing</option>
            <option value="sleep">Sleep</option>
            <option value="shutdown">Shut down</option>
          </select>
        </label>
      </div>
    </div>
  );
}
