import { useCallback } from 'react';
import { ProgressPageView } from '../components/ProgressPageView.tsx';
import { deriveProgressPresentation } from '../model/progressPresentation.ts';
import { useProgressRunControls } from '../runtime/controls/useProgressRunControls.ts';
import { useProgressErrorState } from '../runtime/errors/useProgressErrorState.ts';
import { useProgressRunEvents } from '../runtime/events/useProgressRunEvents.ts';
import { useProgressLaunchLifecycle } from '../runtime/launch/useProgressLaunchLifecycle.ts';
import { useProgressPowerActions } from '../runtime/power/useProgressPowerActions.ts';
import { useProgressPreferences } from '../runtime/preferences/useProgressPreferences.ts';
import { useProgressRunSession } from '../runtime/session/useProgressRunSession.ts';
import { useProgressWindowDestruction } from '../runtime/window/useProgressWindowDestruction.ts';
import {
  useProgressWindowChrome,
  useProgressWindowClose,
  useProgressWindowZoom,
} from '../runtime/window/useProgressWindowLifecycle.ts';

export function ProgressPageController() {
  const session = useProgressRunSession();
  const errors = useProgressErrorState(session.runStateRef, session.requestRender);
  const preferences = useProgressPreferences(errors.reportControlError);
  const windowDestruction = useProgressWindowDestruction(errors.reportControlError);
  const controls = useProgressRunControls({
    runStateRef: session.runStateRef,
    requestRender: session.requestRender,
    reportControlError: errors.reportControlError,
  });
  const powerActions = useProgressPowerActions({
    runStateRef: session.runStateRef,
    autoCloseEnabledRef: preferences.autoCloseEnabledRef,
    whenFinishedActionRef: preferences.whenFinishedActionRef,
    persistAutoCloseEnabled: preferences.changeAutoCloseEnabled,
    persistWhenFinishedAction: preferences.changeWhenFinishedAction,
    requestWindowDestruction: windowDestruction.requestWindowDestruction,
    reportControlError: errors.reportControlError,
  });
  const requestWindowDestruction = useCallback(async () => {
    powerActions.clearTransientActions();
    await windowDestruction.requestWindowDestruction();
  }, [powerActions.clearTransientActions, windowDestruction.requestWindowDestruction]);
  const launch = useProgressLaunchLifecycle({
    runStateRef: session.runStateRef,
    requestRender: session.requestRender,
    resetRunControls: controls.resetRequests,
    resetPowerActions: powerActions.resetForLaunch,
    reportControlError: errors.reportControlError,
  });

  useProgressWindowZoom(preferences.zoomPreference, errors.reportWindowChromeFailure);
  useProgressRunEvents({
    runStateRef: session.runStateRef,
    pendingLaunchRef: launch.pendingLaunchRef,
    pauseRequestRef: controls.pauseRequestRef,
    stopRequestRef: controls.stopRequestRef,
    powerActionReadyRunIdRef: powerActions.powerActionReadyRunIdRef,
    autoCloseEnabledRef: preferences.autoCloseEnabledRef,
    whenFinishedActionRef: preferences.whenFinishedActionRef,
    setRunRejectionMessage: launch.setRunRejectionMessage,
    setPausePending: controls.setPausePending,
    setStopState: controls.setStopState,
    setScheduledAutoClose: powerActions.setScheduledAutoClose,
    setPowerActionCountdown: powerActions.setPowerActionCountdown,
    setPowerActionFailure: powerActions.setPowerActionFailure,
    armLaunch: launch.armLaunch,
    resetRunControlRequests: controls.resetRequests,
    requestRender: session.requestRender,
    requestWindowDestruction,
    reportControlError: errors.reportControlError,
  });

  const presentation = deriveProgressPresentation(
    session.currentRunState,
    launch.runRejectionMessage,
  );
  useProgressWindowChrome({
    runState: session.currentRunState,
    completionPercentage: presentation.completionPercentage,
    runPaused: presentation.runPaused,
    runRejectionMessage: launch.runRejectionMessage,
    reportWindowChromeFailure: errors.reportWindowChromeFailure,
  });
  useProgressWindowClose({
    runStateRef: session.runStateRef,
    pendingLaunchRef: launch.pendingLaunchRef,
    pauseRequestRef: controls.pauseRequestRef,
    stopRequestRef: controls.stopRequestRef,
    closeRequestPendingRef: windowDestruction.closeRequestPendingRef,
    windowDestructionPendingRef: windowDestruction.windowDestructionPendingRef,
    setScheduledAutoClose: powerActions.setScheduledAutoClose,
    setPowerActionCountdown: powerActions.setPowerActionCountdown,
    setPausePending: controls.setPausePending,
    setStopState: controls.setStopState,
    requestRender: session.requestRender,
    requestWindowDestruction,
    reportControlError: errors.reportControlError,
    reportWindowChromeFailure: errors.reportWindowChromeFailure,
  });

  return (
    <ProgressPageView
      runState={session.currentRunState}
      runStateRef={session.runStateRef}
      presentation={presentation}
      runRejectionMessage={launch.runRejectionMessage}
      formatByteRate={session.formatByteRate}
      formatItemRate={session.formatItemRate}
      errorDetailsOpen={errors.errorDetailsOpen}
      onToggleErrorDetails={errors.toggleErrorDetails}
      powerActionCountdown={powerActions.powerActionCountdown}
      countdownTitleId={powerActions.countdownTitleId}
      countdownDescriptionId={powerActions.countdownDescriptionId}
      countdownProgressFillRef={powerActions.countdownProgressFillRef}
      countdownCancelButtonRef={powerActions.countdownCancelButtonRef}
      onCancelPowerActionCountdown={powerActions.cancelPowerActionCountdown}
      powerActionFailure={powerActions.powerActionFailure}
      powerActionPending={powerActions.powerActionPending}
      onRetryPowerAction={(failure) => {
        void powerActions.executePowerAction(failure.runId, failure.action);
      }}
      onDismissPowerActionFailure={() => powerActions.setPowerActionFailure(null)}
      pausePending={controls.pausePending}
      stopState={controls.stopState}
      onTogglePause={() => { void controls.togglePause(); }}
      onStop={() => { void controls.stopRun(); }}
      autoCloseEnabled={preferences.autoCloseEnabled}
      onAutoCloseEnabledChange={powerActions.changeAutoCloseEnabled}
      whenFinishedAction={preferences.whenFinishedAction}
      onWhenFinishedActionChange={powerActions.changeWhenFinishedAction}
    />
  );
}
