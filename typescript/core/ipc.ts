// Every call across the Tauri boundary, in one typed place. Components never touch invoke() directly:
// a command name typo is then a compile error here instead of a rejected promise at click time.

import { invoke } from '@tauri-apps/api/core';
import type { PlanDto, OpDto } from './plan';
import type { ApplyDto } from './types/generated/ApplyDto';
import type { AppSettings } from './types/generated/AppSettings';
import type { CompareOwner } from './types/generated/CompareOwner';
import type { Job as JobFull } from './types/generated/Job';
import type { JobDto } from './types/generated/JobDto';
import type { JobFileSchemaDto } from './types/generated/JobFileSchemaDto';
import type { JunkPresetDto } from './types/generated/JunkPresetDto';
import type { MigrateReport } from './types/generated/MigrateReport';
import type { PathVerdict } from './types/generated/PathVerdict';
import type { PlanHeader } from './types/generated/PlanHeader';
import type { PreflightDto } from './types/generated/PreflightDto';
import type { RowMeta } from './types/generated/RowMeta';
import type { RunRecord } from './types/generated/RunRecord';
import type { SamePage } from './types/generated/SamePage';
import type { SameRow } from './types/generated/SameRow';
import type { SelectedRowDto } from './types/generated/SelectedRowDto';

export type { JobFull, JunkPresetDto };

export type { SameRow, SamePage };

// Jobs

export const listJobs = () => invoke<JobDto[]>('list_jobs');
export const getJob = (name: string) => invoke<JobFull>('get_job', { name });
/// What a new job starts from, straight from the engine's `Job::default()` — junk presets already
/// materialized into `exclude`. Never mirrored in TypeScript: a second copy of engine policy drifts.
export const defaultJob = () => invoke<JobFull>('default_job');
/// The schema in the job file on disk, against the one this build writes. `getJob` returns the migrated
/// job, so this is the only way to tell that the exclude lines on screen are not in the file yet.
export const jobFileSchema = (name: string) => invoke<JobFileSchemaDto>('job_file_schema', { name });
/// Returns the canonical revision of the effective configuration that was written.
export const saveJob = (name: string, job: JobFull) => invoke<string>('save_job', { name, job });
export const deleteJob = (name: string) => invoke<void>('delete_job', { name });
export const jobsDir = () => invoke<string>('jobs_dir');

// Compare / run

/// Capability-degradation consent, per job, per session. A remote backend that lacks something the job
/// asks for (fsync, sampled reads, a reachable trash…) makes the engine REFUSE with the exact list.
/// Never persisted: each session sees the list at least once.
const capsConsent = new Set<string>();

/// invoke(), plus the capability-consent round-trip: on the engine's refusal (its message carries the
/// --accept-caps lines), show the list verbatim and retry once if the user agrees.
async function withCapsConsent<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  const name = String(args.name ?? '');
  try {
    return await invoke<T>(cmd, { ...args, acceptCaps: capsConsent.has(name) });
  } catch (e) {
    const msg = String(e);
    if (!msg.includes('--accept-caps')) throw e;
    const lines = msg.split('\n').slice(1).join('\n').trim();
    const ok = window.confirm(
      `This job degrades on capabilities the remote backend lacks:\n\n${lines}\n\nProceed anyway? (Applies to '${name}' for this session.)`,
    );
    if (!ok) throw e;
    capsConsent.add(name);
    return await invoke<T>(cmd, { ...args, acceptCaps: true });
  }
}

export const compareJob = (name: string, targetIndex: number) =>
  withCapsConsent<PlanDto>('compare_job', { name, targetIndex });

export const applyJob = (
  name: string,
  plan: PlanDto,
  selected: SelectedRowDto[],
  acknowledged: boolean,
  targetIndex: number,
  launchId: number,
) => withCapsConsent<ApplyDto>('apply_job', { name, plan, selected, acknowledged, targetIndex, launchId });

/// AutoScan never grants capability consent by itself — it only reuses consent the user already gave
/// interactively this session (a degraded run must never start on a timer without a human having seen the list)
export const applyJobUnattended = (name: string, plan: PlanDto, selected: SelectedRowDto[], targetIndex: number) =>
  invoke<ApplyDto>('apply_job', {
    name, plan, selected, acknowledged: false, targetIndex, acceptCaps: capsConsent.has(name),
  });

export const preflight = (name: string, plan: PlanDto, selected: SelectedRowDto[], acknowledged: boolean, targetIndex: number) =>
  invoke<PreflightDto>('preflight', { name, plan, selected, acknowledged, targetIndex });

export const cancelRun = () => invoke<boolean>('cancel_run');
export const pauseRun = (paused: boolean) => invoke<void>('pause_run', { paused });
export const openProgressWindow = () => invoke<number>('open_progress_window');
export const cancelProgressLaunch = (launchId: number) => invoke<boolean>('cancel_progress_launch', { launchId });
export const closeProgressLaunch = () => invoke<'pending' | 'active' | 'none'>('close_progress_launch');
export const destroyProgressWindow = () => invoke<void>('close_progress_window');
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

export const listSame = (owner: CompareOwner, query: string, offset: number, limit: number) =>
  invoke<SamePage>('list_same', { owner, query, offset, limit });

export const exportCsv = (path: string, header: PlanHeader, ops: OpDto[], metas: RowMeta[], checked: boolean[]) =>
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
export const saveSettings = (s: AppSettings) => invoke<MigrateReport>('save_settings', { s, migrate: true });
