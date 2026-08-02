import { invoke } from '@tauri-apps/api/core';

import type { RunEventEnvelope } from '#core/domain/runs/runEvents.ts';
import type { PostRunPowerActionDto } from '#core/types/generated/PostRunPowerActionDto.ts';
import type { ProgressWindowCloseDecisionDto } from '#core/types/generated/ProgressWindowCloseDecisionDto.ts';

export const cancelApplyRun = (runId: number) => invoke<boolean>('cancel_apply_run', { runId });
export const setApplyPaused = (runId: number, paused: boolean) =>
  invoke<boolean>('set_apply_paused', { runId, paused });
export const replayApplyEvents = (afterSequence = 0) =>
  invoke<RunEventEnvelope[]>('replay_apply_events', { afterSequence });
export const reportProgressWindowMounted = (launchId: number) =>
  invoke<void>('report_progress_window_mounted', { launchId });
export const acknowledgeProgressLaunch = (launchId: number) =>
  invoke<void>('acknowledge_progress_launch', { launchId });
export const beginProgressWindowClose = () =>
  invoke<ProgressWindowCloseDecisionDto>('begin_progress_window_close');
export const destroyProgressWindow = () => invoke<void>('destroy_progress_window');
export const executePostRunPowerAction = (runId: number, action: PostRunPowerActionDto) =>
  invoke<void>('execute_post_run_power_action', { runId, action });
