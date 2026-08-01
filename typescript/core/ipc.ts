// Every call across the Tauri boundary, in one typed place. Components never touch invoke() directly:
// a command name typo is then a compile error here instead of a rejected promise at click time.

import { invoke } from '@tauri-apps/api/core';
import type { PlanDto, PlanOperation } from './plan';
import type { RunEventEnvelope } from './runEvents';
import {
  applyAuthorizationArgs,
  approveOperationArgs,
  autoScanApplyAuthorizationArgs,
  compareAuthorizationArgs,
  reviewApplyArgs,
  reviewCompareArgs,
  startAutoScanArgs,
} from './operationProtocol';
import type { ApplyDto } from './types/generated/ApplyDto';
import type { AppSettings } from './types/generated/AppSettings';
import type { AuthorizationDto } from './types/generated/AuthorizationDto';
import type { AutoScanStatusDto } from './types/generated/AutoScanStatusDto';
import type { AutoScanTriggerDto } from './types/generated/AutoScanTriggerDto';
import type { CompareOwner } from './types/generated/CompareOwner';
import type { CompareIdentity } from './types/generated/CompareIdentity';
import type { Job as JobFull } from './types/generated/Job';
import type { JobDeleteDto } from './types/generated/JobDeleteDto';
import type { JobDetailDto } from './types/generated/JobDetailDto';
import type { JobDto } from './types/generated/JobDto';
import type { JobFileSchemaDto } from './types/generated/JobFileSchemaDto';
import type { JobSaveDto } from './types/generated/JobSaveDto';
import type { JunkPresetDto } from './types/generated/JunkPresetDto';
import type { MigrateReport } from './types/generated/MigrateReport';
import type { OperationReviewDto } from './types/generated/OperationReviewDto';
import type { OperationApprovalDto } from './types/generated/OperationApprovalDto';
import type { AutoScanCompareRequestDto } from './types/generated/AutoScanCompareRequestDto';
import type { PathVerdict } from './types/generated/PathVerdict';
import type { PlanHeader } from './types/generated/PlanHeader';
import type { ProgressWindowCloseDecisionDto } from './types/generated/ProgressWindowCloseDecisionDto';
import type { RowMeta } from './types/generated/RowMeta';
import type { RunRecord } from './types/generated/RunRecord';
import type { IdenticalPage } from './types/generated/IdenticalPage';
import type { IdenticalRow } from './types/generated/IdenticalRow';
import type { SelectedRowDto } from './types/generated/SelectedRowDto';

export type { AuthorizationDto, JobDeleteDto, JobDetailDto, JobFull, JobSaveDto, JunkPresetDto, OperationReviewDto };

export type { IdenticalRow, IdenticalPage };

// Jobs

export const listJobs = () => invoke<JobDto[]>('list_jobs');
export const getJob = (name: string) => invoke<JobDetailDto>('get_job', { name });
/// What a new job starts from, straight from the engine's `Job::default()` — junk presets already
/// materialized into `exclude`. Never mirrored in TypeScript: a second copy of engine policy drifts.
export const defaultJob = () => invoke<JobFull>('default_job');
/// The schema in the job file on disk, against the one this build writes. `getJob` returns the migrated
/// job, so this is the only way to tell that the exclude lines on screen are not in the file yet.
export const jobFileSchema = (name: string) => invoke<JobFileSchemaDto>('job_file_schema', { name });
export interface ExistingJobRevision {
  originalName: string;
  expectedRevision: string;
}

export const saveJob = (name: string, job: JobFull, existing?: ExistingJobRevision) => invoke<JobSaveDto>('save_job', {
  name,
  job,
  originalName: existing?.originalName,
  expectedRevision: existing?.expectedRevision,
});
export const deleteJob = (name: string, expectedJobId: string, expectedRevision: string) =>
  invoke<JobDeleteDto>('delete_job', { name, expectedJobId, expectedRevision });
export const jobsDir = () => invoke<string>('jobs_dir');

export const startAutoScan = (expectedJobId: string, expectedRevision: string, targetIndex: number) =>
  invoke<AutoScanStatusDto>('start_autoscan', startAutoScanArgs(expectedJobId, expectedRevision, targetIndex));
export const stopAutoScan = () => invoke<AutoScanStatusDto>('stop_autoscan');
export const autoScanStatus = () => invoke<AutoScanStatusDto>('autoscan_status');
export const completeAutoScan = (
  generation: number,
  ticketId: number,
  succeeded: boolean,
) => invoke<AutoScanStatusDto>('complete_autoscan', {
  generation, ticketId, succeeded,
});

// Compare / run

export const reviewCompare = (
  expectedJobId: string,
  targetIndex?: number,
  autoScanRequest?: AutoScanCompareRequestDto,
) => invoke<OperationReviewDto>(
  'review_compare',
  reviewCompareArgs(expectedJobId, targetIndex, autoScanRequest),
);

export const approveOperation = (
  challengeId: string,
  approval: OperationApprovalDto,
) => invoke<AuthorizationDto>('approve_operation', approveOperationArgs(challengeId, approval));

export const compareJob = (authorizationToken: string) =>
  invoke<PlanDto>('compare_job', compareAuthorizationArgs(authorizationToken));

export const touchCompare = (owner: CompareOwner) =>
  invoke<CompareOwner | null>('touch_compare', { owner });

export const restoreCompare = (jobId: string, targetIndex: number) =>
  invoke<PlanDto | null>('restore_compare', { jobId, targetIndex });

export const reviewApply = (compareIdentity: CompareIdentity, selected: SelectedRowDto[]) =>
  invoke<OperationReviewDto>('review_apply', reviewApplyArgs(compareIdentity, selected));

export const authorizeAutoScanApply = (generation: number, ticketId: number) =>
  invoke<AuthorizationDto>('authorize_autoscan_apply', autoScanApplyAuthorizationArgs(generation, ticketId));

export const applyJob = (authorizationToken: string, launchId?: number) =>
  invoke<ApplyDto>('apply_job', applyAuthorizationArgs(authorizationToken, launchId));

export const cancelRun = (runId: number) => invoke<boolean>('cancel_run', { runId });
export const pauseRun = (runId: number, paused: boolean) => invoke<boolean>('pause_run', { runId, paused });
export const replayRunEvents = (purpose: 'compare' | 'apply', afterSequence = 0) =>
  invoke<RunEventEnvelope[]>('replay_run_events', { purpose, afterSequence });
export const openProgressWindow = () => invoke<number>('open_progress_window');
export const cancelProgressLaunch = (launchId: number) => invoke<boolean>('cancel_progress_launch', { launchId });
export const beginProgressWindowClose = () =>
  invoke<ProgressWindowCloseDecisionDto>('begin_progress_window_close');
export const destroyProgressWindow = () => invoke<void>('destroy_progress_window');
export const postSyncAction = (kind: string) => invoke<void>('post_sync_action', { kind });

// Paths / filters / export

export const inspectPaths = (source: string, target: string) =>
  invoke<PathVerdict>('inspect_paths', { source, target });

/// Mask matching goes to Rust: the frontend never writes its own glob, or a mask that worked in the UI
/// could behave differently once written into the job's exclude list.
export const maskMatch = (masks: string[], paths: string[]) =>
  invoke<boolean[]>('mask_match', { masks, paths });

/// The junk exclude presets, patterns and all. Fetched rather than hard-coded for the same reason as
/// maskMatch: the checkbox writes the very strings the engine would apply, so it can honestly claim to
/// describe what the job excludes.
export const junkPresets = () => invoke<JunkPresetDto[]>('junk_presets');

export const listIdentical = (owner: CompareOwner, query: string, offset: number, limit: number) =>
  invoke<IdenticalPage>('list_identical', { owner, query, offset, limit });

export const exportCsv = (path: string, header: PlanHeader, ops: PlanOperation[], metas: RowMeta[], checked: boolean[]) =>
  invoke<number>('export_csv', { path, header, ops, metas, checked });

export const reveal = (path: string) => invoke<void>('reveal', { path });

/// Native dialog. The @tauri-apps/plugin-dialog npm package is just a wrapper around this one invoke,
/// so calling IPC directly saves a frontend dependency (tauri_plugin_dialog is already registered on the Rust side).
export async function pickPath(opts: { directory?: boolean; save?: boolean; title: string; defaultPath?: string }): Promise<string | null> {
  const { directory, save, title, defaultPath } = opts;
  const r = await invoke<unknown>(save ? 'plugin:dialog|save' : 'plugin:dialog|open', {
    options: { title, defaultPath: defaultPath || undefined, directory: !!directory, multiple: false, recursive: false },
  });
  if (!r) return null;
  // Depending on the patch version, open may return string | string[] | {path}
  const one = Array.isArray(r) ? r[0] : r;
  if (typeof one === 'string') return one;
  if (one && typeof one === 'object' && typeof (one as { path?: string }).path === 'string') return (one as { path: string }).path;
  return null;
}

// Logs / settings

export const lastSyncs = () => invoke<Record<string, RunRecord>>('last_syncs');
export const logRuns = (job: string | null, limit: number) => invoke<RunRecord[]>('log_runs', { job, limit });
/// runId is the run **directory name**, not an index — compare-class runs have none (see RunRecord.run_id)
export const logArtifact = (runId: string, which: string) => invoke<string[]>('log_artifact', { runId, which });
export const logDirPath = (runId: string | null) => invoke<string>('log_dir_path', { runId });
export const getSettings = () => invoke<AppSettings>('get_settings');
export const saveSettings = (settings: AppSettings) => invoke<MigrateReport>('save_settings', { settings, migrate: true });
