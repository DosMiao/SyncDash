import type { PowerAction } from '#core/application/progress/postRunActions.ts';

export interface PendingLaunch {
  launchId: number;
  afterRunId: number;
}

export interface PauseRequest {
  runId: number;
  pause: boolean;
}

export interface StopRequest {
  runId: number;
}

export interface PowerActionRequest {
  runId: number;
  action: PowerAction;
}
