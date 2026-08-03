import {
  acknowledgeProgressLaunch,
  cancelApplyRun,
  destroyProgressWindow,
  executePostRunPowerAction,
  setApplyPaused,
} from '#core/infrastructure/tauri/commands/progress.ts';

/** The progress page's complete imperative platform surface. */
export const progressPlatformActions = {
  acknowledgeLaunch: acknowledgeProgressLaunch,
  cancelRun: cancelApplyRun,
  destroyWindow: destroyProgressWindow,
  executePowerAction: executePostRunPowerAction,
  setPaused: setApplyPaused,
} as const;
