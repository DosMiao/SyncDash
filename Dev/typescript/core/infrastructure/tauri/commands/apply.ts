import { invoke } from '@tauri-apps/api/core';

import { applyAuthorizationArgs } from '#core/application/operations/operationProtocol.ts';
import type { ApplyDto } from '#core/types/generated/ApplyDto.ts';

export const applyJob = (authorizationToken: string, launchId?: number) =>
  invoke<ApplyDto>('apply_job', applyAuthorizationArgs(authorizationToken, launchId));

export const openProgressWindow = () => invoke<number>('open_progress_window');
export const cancelProgressLaunch = (launchId: number) =>
  invoke<boolean>('cancel_progress_launch', { launchId });
