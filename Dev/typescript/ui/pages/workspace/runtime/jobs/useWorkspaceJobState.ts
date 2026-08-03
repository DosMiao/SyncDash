import type { JobSaveDto } from '#core/types/generated/JobSaveDto.ts';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { Dispatch } from 'react';
import * as jobsIpc from '#core/infrastructure/tauri/commands/jobs.ts';
import * as logsIpc from '#core/infrastructure/tauri/commands/logs.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import type { RootEditorAction } from '#core/application/jobs/rootEditor.ts';
import { RequestFence } from '#core/application/coordination/requestFence.ts';
import { addPathToHistory, loadPathHistory, savePathHistory } from '#core/infrastructure/preferences/pathHistory.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { JobDetailDto } from '#core/types/generated/JobDetailDto.ts';
import type { RunRecord } from '#core/types/generated/RunRecord.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import type { JobIdentitySnapshot } from '../../model/workspacePageModel.ts';
import { useWorkspaceBootstrap } from '../lifecycle/useWorkspaceBootstrap.ts';

/**
 * How a mutation announcement must be worded once the job registry has been re-read. A failed
 * refresh is never swallowed: callers append `suffix` to their message and must raise the error
 * tone whenever `failed`, because the mutation itself landed while the job list the operator is
 * looking at may no longer describe it.
 */
export interface JobListRefreshOutcome {
  suffix: string;
  failed: boolean;
}

interface WorkspaceJobStateOptions {
  selectedJob: JobDto | null;
  refreshJobs: () => Promise<JobDto[]>;
  dispatchCompare: Dispatch<CompareWorkspaceAction>;
  dispatchRootEditor: Dispatch<RootEditorAction>;
  setStatus: StatusApi['setMessage'];
}

export function useWorkspaceJobState({
  selectedJob,
  refreshJobs,
  dispatchCompare,
  dispatchRootEditor,
  setStatus,
}: WorkspaceJobStateOptions) {
  // One payload, one state: `get_job` answers both what the job is configured to do and what the
  // engine derived from its target phrases, and the pill row states the two side by side. Holding
  // them apart would let the row mix a fresh configuration with a stale peer verdict.
  const [jobDetail, setJobDetail] = useState<JobDetailDto | null>(null);
  const [latestRunByJobId, setLatestRunByJobId] = useState<Record<string, RunRecord>>({});
  const latestRunSummaryFence = useRef(new RequestFence());
  const [appVersion, setAppVersion] = useState('');
  const [jobsDir, setJobsDir] = useState('');
  const [initialPathHistoryLoad] = useState(() => loadPathHistory(localStorage));
  const [pathHistory, setPathHistory] = useState<string[]>(initialPathHistoryLoad.paths);
  const pathHistoryRef = useRef(pathHistory);
  pathHistoryRef.current = pathHistory;

  useEffect(() => {
    if (initialPathHistoryLoad.warning) setStatus(initialPathHistoryLoad.warning, 'err');
  }, [initialPathHistoryLoad.warning, setStatus]);

  const pushHistory = useCallback((candidatePath: string) => {
    const nextPaths = addPathToHistory(pathHistoryRef.current, candidatePath);
    if (nextPaths === pathHistoryRef.current) return;
    pathHistoryRef.current = nextPaths;
    setPathHistory(nextPaths);
    const warning = savePathHistory(localStorage, nextPaths);
    if (warning) setStatus(warning, 'err');
  }, [setStatus]);

  // Retained Compare evidence is bound to a job identity and its configuration revision. Each
  // reconciler applies its own guard for what "the job as it exists now" means and then delegates
  // here; a missing job expires as a deletion and a moved revision expires as a change. There is
  // deliberately no default branch: an unchanged identity must never invalidate evidence.
  const expireJobExecutionIfIdentityChanged = useCallback((
    previous: JobIdentitySnapshot,
    nextJobId: string | null,
    nextConfigRevision: string | null,
  ) => {
    if (nextJobId !== previous.jobId) {
      dispatchCompare({ type: 'job_execution_expired', jobId: previous.jobId, reason: 'job_deleted' });
    } else if (nextConfigRevision !== previous.configRevision) {
      dispatchCompare({
        type: 'job_execution_expired',
        jobId: previous.jobId,
        configRevision: previous.configRevision,
        reason: 'job_changed',
      });
    }
  }, [dispatchCompare]);

  const reconcileWorkspaceJob = useCallback((previous: JobIdentitySnapshot, refreshedJob: JobDto | null) => {
    expireJobExecutionIfIdentityChanged(
      previous,
      refreshedJob?.job_id ?? null,
      refreshedJob?.config_revision ?? null,
    );
  }, [expireJobExecutionIfIdentityChanged]);

  const reconcileSavedWorkspaceJob = useCallback((
    saved: JobSaveDto,
    previous: JobIdentitySnapshot | null,
  ) => {
    if (!previous || saved.effect === 'created' || saved.effect === 'no_op') return;
    expireJobExecutionIfIdentityChanged(previous, saved.job_id, saved.config_revision);
  }, [expireJobExecutionIfIdentityChanged]);

  const refreshLatestRunSummaries = useCallback(() => {
    const ticket = latestRunSummaryFence.current.start('latest-run-summaries');
    logsIpc.latestRunRecords().then(
      (latestRuns) => {
        if (latestRunSummaryFence.current.owns(ticket)) {
          setLatestRunByJobId(Object.fromEntries(
            latestRuns.map(({ job_id: jobId, record }) => [jobId, record]),
          ));
        }
      },
      (error: unknown) => {
        if (latestRunSummaryFence.current.owns(ticket)) {
          setStatus(`Could not refresh the latest-run indicators: ${error}`, 'err');
        }
      },
    );
  }, [setStatus]);

  const describeMutationFailure = useCallback(async (
    action: string,
    error: unknown,
  ): Promise<string> => {
    try {
      await refreshJobs();
      return `${action}: ${error} · refreshed the job registry; no unseen changes were overwritten`;
    } catch (refreshError) {
      return `${action}: ${error} · job-registry refresh failed: ${refreshError}`;
    }
  }, [refreshJobs]);

  const refreshJobsForAnnouncement = useCallback(async (): Promise<JobListRefreshOutcome> => {
    try {
      await refreshJobs();
      return { suffix: '', failed: false };
    } catch (error) {
      return { suffix: ` · job-list refresh failed: ${error}`, failed: true };
    }
  }, [refreshJobs]);

  const expireDeletedJobState = useCallback((jobId: string) => {
    dispatchCompare({ type: 'job_execution_expired', jobId, reason: 'job_deleted' });
    dispatchRootEditor({ type: 'job_removed', jobId });
  }, [dispatchCompare, dispatchRootEditor]);

  useWorkspaceBootstrap({ refreshJobs, refreshLatestRunSummaries, setJobsDir, setAppVersion, setStatus });

  // The sole writer of the detail snapshot. Every mutation re-reads the registry, which hands back
  // a fresh `selectedJob`, so this effect re-runs and the derived peer verdict is never carried
  // over from the phrase the job had before the save.
  useEffect(() => {
    if (!selectedJob) { setJobDetail(null); return; }
    let live = true;
    jobsIpc.getJob(selectedJob.name).then((detail) => {
      if (live) setJobDetail(detail);
    }).catch((error) => {
      if (!live) return;
      setJobDetail(null);
      setStatus(`Failed to load '${selectedJob.name}' settings: ${error}`, 'err');
    });
    return () => { live = false; };
  }, [selectedJob, setStatus]);

  return {
    appVersion,
    describeMutationFailure,
    expireDeletedJobState,
    jobConfiguration: jobDetail?.job ?? null,
    jobPeerLink: jobDetail?.peer_link ?? null,
    jobsDir,
    latestRunByJobId,
    pathHistory,
    pushHistory,
    reconcileSavedWorkspaceJob,
    reconcileWorkspaceJob,
    refreshJobsForAnnouncement,
    refreshLatestRunSummaries,
  };
}
