import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import type { MutableRefObject } from 'react';
import {
  deriveAutoCloseRequest,
  derivePowerActionCountdown,
} from '#core/application/progress/postRunActions.ts';
import type {
  AutoCloseRequest,
  PowerAction,
  PowerActionCountdown,
  WhenFinishedAction,
} from '#core/application/progress/postRunActions.ts';
import type { RunState } from '#core/application/progress/runstate.ts';
import type { PowerActionFailure } from '../../model/progressPresentation.ts';
import { progressPlatformActions } from '../actions/progressPlatformActions.ts';
import type { PowerActionRequest } from '../progressRuntimeTypes.ts';

interface ProgressPowerActionsOptions {
  runStateRef: MutableRefObject<RunState>;
  autoCloseEnabledRef: MutableRefObject<boolean>;
  whenFinishedActionRef: MutableRefObject<WhenFinishedAction>;
  persistAutoCloseEnabled: (enabled: boolean) => boolean;
  persistWhenFinishedAction: (action: string) => WhenFinishedAction | null;
  requestWindowDestruction: () => Promise<void>;
  reportControlError: (action: string, error: unknown) => void;
}

/** Owns post-run countdown, auto-close, power-action authorization, and retry state. */
export function useProgressPowerActions({
  runStateRef,
  autoCloseEnabledRef,
  whenFinishedActionRef,
  persistAutoCloseEnabled,
  persistWhenFinishedAction,
  requestWindowDestruction,
  reportControlError,
}: ProgressPowerActionsOptions) {
  const [powerActionCountdown, setPowerActionCountdown] = useState<PowerActionCountdown | null>(null);
  const [scheduledAutoClose, setScheduledAutoClose] = useState<AutoCloseRequest | null>(null);
  const [powerActionFailure, setPowerActionFailure] = useState<PowerActionFailure | null>(null);
  const powerActionRequestRef = useRef<PowerActionRequest | null>(null);
  const [powerActionPending, setPowerActionPending] = useState<PowerAction | null>(null);
  const powerActionReadyRunIdRef = useRef<number | null>(null);
  const countdownProgressFillRef = useRef<HTMLDivElement>(null);
  const countdownCancelButtonRef = useRef<HTMLButtonElement>(null);
  const countdownTitleId = useId();
  const countdownDescriptionId = useId();

  const clearTransientActions = useCallback(() => {
    setScheduledAutoClose(null);
    setPowerActionCountdown(null);
  }, []);

  const resetForLaunch = useCallback(() => {
    clearTransientActions();
    setPowerActionFailure(null);
    powerActionReadyRunIdRef.current = null;
  }, [clearTransientActions]);

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
      await progressPlatformActions.executePowerAction(runId, action);
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
  }, [autoCloseEnabledRef, reportControlError, runStateRef, whenFinishedActionRef]);

  const reconcileCountdown = useCallback((
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
  }, [runStateRef]);

  const changeAutoCloseEnabled = useCallback((nextAutoCloseEnabled: boolean) => {
    if (!persistAutoCloseEnabled(nextAutoCloseEnabled)) return;
    setScheduledAutoClose(null);
    setPowerActionFailure(null);
    reconcileCountdown(whenFinishedActionRef.current, nextAutoCloseEnabled);
  }, [persistAutoCloseEnabled, reconcileCountdown, whenFinishedActionRef]);

  const changeWhenFinishedAction = useCallback((nextWhenFinishedAction: string) => {
    const savedAction = persistWhenFinishedAction(nextWhenFinishedAction);
    if (!savedAction) return;
    setScheduledAutoClose(null);
    setPowerActionFailure(null);
    reconcileCountdown(savedAction, autoCloseEnabledRef.current);
  }, [autoCloseEnabledRef, persistWhenFinishedAction, reconcileCountdown]);

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
      setPowerActionCountdown(null);
      void requestWindowDestruction();
    }, 1200);
    return () => window.clearTimeout(autoCloseTimerId);
  }, [autoCloseEnabledRef, requestWindowDestruction, runStateRef, scheduledAutoClose]);

  return {
    cancelPowerActionCountdown,
    changeAutoCloseEnabled,
    changeWhenFinishedAction,
    clearTransientActions,
    countdownCancelButtonRef,
    countdownDescriptionId,
    countdownProgressFillRef,
    countdownTitleId,
    executePowerAction,
    powerActionCountdown,
    powerActionFailure,
    powerActionPending,
    powerActionReadyRunIdRef,
    resetForLaunch,
    scheduledAutoClose,
    setPowerActionCountdown,
    setPowerActionFailure,
    setScheduledAutoClose,
  };
}
