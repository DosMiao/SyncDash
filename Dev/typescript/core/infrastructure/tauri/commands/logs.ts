import { invoke } from '@tauri-apps/api/core';

import type { LatestRunRecord } from '#core/types/generated/LatestRunRecord.ts';
import type { LogArtifactKind } from '#core/types/generated/LogArtifactKind.ts';
import type { RunRecord } from '#core/types/generated/RunRecord.ts';

export const latestRunRecords = () => invoke<LatestRunRecord[]>('latest_run_records');
export const logRuns = (jobId: string | null) => invoke<RunRecord[]>('log_runs', { jobId });
export const logArtifact = (recordId: string, artifact: LogArtifactKind) =>
  invoke<string[]>('log_artifact', { recordId, artifact });
export const revealLogLocation = (recordId: string | null) =>
  invoke<void>('reveal_log_location', { recordId });
