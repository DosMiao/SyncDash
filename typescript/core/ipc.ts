// Every call across the Tauri boundary lives here so command names, arguments, and return types have
// one review point. The IPC parity test keeps this runtime string registry aligned with Rust and ACLs.

import { invoke } from '@tauri-apps/api/core';
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
import type { OpenDialogOptions, SaveDialogOptions } from '@tauri-apps/plugin-dialog';
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
import type { CompareIdentity } from './types/generated/CompareIdentity';
import type { CompareFileSideDto } from './types/generated/CompareFileSideDto';
import type { CompareResultForgetDto } from './types/generated/CompareResultForgetDto';
import type { CompareWorkspaceLookupDto } from './types/generated/CompareWorkspaceLookupDto';
import type { CompareWorkspaceSnapshotDto } from './types/generated/CompareWorkspaceSnapshotDto';
import type { CsvExportDto } from './types/generated/CsvExportDto';
import type { CsvRowPresentationDto } from './types/generated/CsvRowPresentationDto';
import type { Job as JobFull } from './types/generated/Job';
import type { JobDeleteDto } from './types/generated/JobDeleteDto';
import type { JobDetailDto } from './types/generated/JobDetailDto';
import type { JobDto } from './types/generated/JobDto';
import type { JobFileSchemaDto } from './types/generated/JobFileSchemaDto';
import type { JobRootField } from './types/generated/JobRootField';
import type { JobRootMutationDto } from './types/generated/JobRootMutationDto';
import type { JobSaveDto } from './types/generated/JobSaveDto';
import type { JunkPresetDto } from './types/generated/JunkPresetDto';
import type { LogArtifactKind } from './types/generated/LogArtifactKind';
import type { LogDirectorySelectionDto } from './types/generated/LogDirectorySelectionDto';
import type { LatestRunRecord } from './types/generated/LatestRunRecord';
import type { OperationReviewDto } from './types/generated/OperationReviewDto';
import type { OperationApprovalDto } from './types/generated/OperationApprovalDto';
import type { AutoScanCompareRequestDto } from './types/generated/AutoScanCompareRequestDto';
import type { PathVerdict } from './types/generated/PathVerdict';
import type { PostRunPowerActionDto } from './types/generated/PostRunPowerActionDto';
import type { ProgressWindowCloseDecisionDto } from './types/generated/ProgressWindowCloseDecisionDto';
import type { RunRecord } from './types/generated/RunRecord';
import type { IdenticalPage } from './types/generated/IdenticalPage';
import type { IdenticalRow } from './types/generated/IdenticalRow';
import type { ReviewedRowDecisionDto } from './types/generated/ReviewedRowDecisionDto';
import type { SettingsSaveDto } from './types/generated/SettingsSaveDto';
import type { SettingsSnapshotDto } from './types/generated/SettingsSnapshotDto';

export type {
  AuthorizationDto,
  JobDeleteDto,
  JobDetailDto,
  JobFull,
  JobRootField,
  JobRootMutationDto,
  JobSaveDto,
  JunkPresetDto,
  OperationReviewDto,
};

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
export const updateJobRoot = (
  name: string,
  expectedJobId: string,
  expectedConfigRevision: string,
  targetIndex: number,
  field: JobRootField,
  value: string,
) => invoke<JobRootMutationDto>('update_job_root', {
  name,
  expectedJobId,
  expectedConfigRevision,
  targetIndex,
  field,
  value,
});
export const swapJobRoots = (
  name: string,
  expectedJobId: string,
  expectedConfigRevision: string,
  targetIndex: number,
) => invoke<JobRootMutationDto>('swap_job_roots', {
  name,
  expectedJobId,
  expectedConfigRevision,
  targetIndex,
});
export const deleteJob = (name: string, expectedJobId: string, expectedRevision: string) =>
  invoke<JobDeleteDto>('delete_job', { name, expectedJobId, expectedRevision });
export const jobsDir = () => invoke<string>('jobs_dir');

export const startAutoScan = (expectedJobId: string, expectedRevision: string, targetIndex: number) =>
  invoke<AutoScanStatusDto>('start_autoscan', startAutoScanArgs(expectedJobId, expectedRevision, targetIndex));
export const stopAutoScan = () => invoke<AutoScanStatusDto>('stop_autoscan');
export const autoScanStatus = () => invoke<AutoScanStatusDto>('autoscan_status');
export const declineAutoScanTrigger = (
  generation: number,
  ticketId: number,
) => invoke<AutoScanStatusDto>('decline_autoscan_trigger', {
  generation, ticketId,
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
  invoke<CompareWorkspaceSnapshotDto>('compare_job', compareAuthorizationArgs(authorizationToken));

export const reconcileCompareWorkspace = (compareIdentity: CompareIdentity) =>
  invoke<CompareWorkspaceLookupDto>('reconcile_compare_workspace', { compareIdentity });

export const restoreCompare = (jobId: string, targetIndex: number, expectedConfigRevision: string) =>
  invoke<CompareWorkspaceLookupDto>('restore_compare', { jobId, targetIndex, expectedConfigRevision });

export const forgetCompareResult = (compareIdentity: CompareIdentity) =>
  invoke<CompareResultForgetDto>('forget_compare_result', { compareIdentity });

export const reviewApply = (
  compareIdentity: CompareIdentity,
  reviewedRowDecisions: ReviewedRowDecisionDto[],
) => invoke<OperationReviewDto>(
  'review_apply',
  reviewApplyArgs(compareIdentity, reviewedRowDecisions),
);

export const authorizeAutoScanApply = (generation: number, ticketId: number) =>
  invoke<AuthorizationDto>('authorize_autoscan_apply', autoScanApplyAuthorizationArgs(generation, ticketId));

export const applyJob = (authorizationToken: string, launchId?: number) =>
  invoke<ApplyDto>('apply_job', applyAuthorizationArgs(authorizationToken, launchId));

export const cancelCompareRun = (runId: number) => invoke<boolean>('cancel_compare_run', { runId });
export const cancelApplyRun = (runId: number) => invoke<boolean>('cancel_apply_run', { runId });
export const setApplyPaused = (runId: number, paused: boolean) =>
  invoke<boolean>('set_apply_paused', { runId, paused });
export const replayCompareEvents = (afterSequence = 0) =>
  invoke<RunEventEnvelope[]>('replay_compare_events', { afterSequence });
export const replayApplyEvents = (afterSequence = 0) =>
  invoke<RunEventEnvelope[]>('replay_apply_events', { afterSequence });
export const openProgressWindow = () => invoke<number>('open_progress_window');
export const cancelProgressLaunch = (launchId: number) => invoke<boolean>('cancel_progress_launch', { launchId });
export const reportProgressWindowMounted = (launchId: number) =>
  invoke<void>('report_progress_window_mounted', { launchId });
export const acknowledgeProgressLaunch = (launchId: number) =>
  invoke<void>('acknowledge_progress_launch', { launchId });
export const beginProgressWindowClose = () =>
  invoke<ProgressWindowCloseDecisionDto>('begin_progress_window_close');
export const destroyProgressWindow = () => invoke<void>('destroy_progress_window');
export const executePostRunPowerAction = (runId: number, action: PostRunPowerActionDto) =>
  invoke<void>('execute_post_run_power_action', { runId, action });

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

export const listIdentical = (compareIdentity: CompareIdentity, query: string, offset: number, limit: number) =>
  invoke<IdenticalPage>('list_identical', { compareIdentity, query, offset, limit });

export const exportCompareCsv = (compareIdentity: CompareIdentity, rows: CsvRowPresentationDto[]) =>
  invoke<CsvExportDto>('export_compare_csv', { compareIdentity, rows });

export const revealCompareRow = (
  compareIdentity: CompareIdentity,
  index: number,
  side: CompareFileSideDto,
  directionReversed: boolean,
) => invoke<void>('reveal_compare_row', { compareIdentity, index, side, directionReversed });

export const revealCsvExport = (receiptId: string) =>
  invoke<void>('reveal_csv_export', { receiptId });

export type DirectoryPickerOptions = Omit<OpenDialogOptions, 'directory' | 'multiple'>;

export const pickDirectory = (options: DirectoryPickerOptions): Promise<string | null> => (
  openDialog({ ...options, directory: true, multiple: false })
);

export const pickSavePath = (options: SaveDialogOptions): Promise<string | null> => saveDialog(options);

// Logs / settings

export const latestRunRecords = () => invoke<LatestRunRecord[]>('latest_run_records');
export const logRuns = (jobId: string | null) => invoke<RunRecord[]>('log_runs', { jobId });
export const logArtifact = (recordId: string, artifact: LogArtifactKind) =>
  invoke<string[]>('log_artifact', { recordId, artifact });
export const revealLogLocation = (recordId: string | null) =>
  invoke<void>('reveal_log_location', { recordId });
export const getSettings = () => invoke<SettingsSnapshotDto>('get_settings');
export const pickLogDirectory = (expectedRevision: string) =>
  invoke<LogDirectorySelectionDto | null>('pick_log_directory', { expectedRevision });
export const saveSettings = (
  settings: AppSettings,
  expectedRevision: string,
  logDirectoryGrant?: string,
) => invoke<SettingsSaveDto>('save_settings', { settings, expectedRevision, logDirectoryGrant });
