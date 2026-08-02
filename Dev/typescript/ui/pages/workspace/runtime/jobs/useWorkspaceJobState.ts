import { useCallback, useEffect, useRef, useState } from 'react';
import type { Dispatch } from 'react';
import * as ipc from '#core/infrastructure/tauri/commands/main.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import type { RootEditorAction } from '#core/application/jobs/rootEditor.ts';
import { RequestFence } from '#core/application/coordination/requestFence.ts';
import { addPathToHistory, loadPathHistory, savePathHistory } from '#core/infrastructure/preferences/pathHistory.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { Job as JobFull } from '#core/types/generated/Job.ts';
import type { RunRecord } from '#core/types/generated/RunRecord.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import type { JobIdentitySnapshot } from '../../model/workspacePageModel.ts';
import { useWorkspaceBootstrap } from '../lifecycle/useWorkspaceBootstrap.ts';

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
  const [jobConfiguration, setJobConfiguration] = useState<JobFull | null>(null);
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

  const reconcileWorkspaceJob = useCallback((previous: JobIdentitySnapshot, refreshedJob: JobDto | null) => {
    if (!refreshedJob || refreshedJob.job_id !== previous.jobId) {
      dispatchCompare({ type: 'job_execution_expired', jobId: previous.jobId, reason: 'job_deleted' });
    } else if (refreshedJob.config_revision !== previous.configRevision) {
      dispatchCompare({
        type: 'job_execution_expired',
        jobId: previous.jobId,
        configRevision: previous.configRevision,
        reason: 'job_changed',
      });
    }
  }, [dispatchCompare]);

  const reconcileSavedWorkspaceJob = useCallback((
    saved: ipc.JobSaveDto,
    previous: JobIdentitySnapshot | null,
  ) => {
    if (!previous || saved.effect === 'created' || saved.effect === 'no_op') return;
    if (saved.job_id !== previous.jobId) {
      dispatchCompare({ type: 'job_execution_expired', jobId: previous.jobId, reason: 'job_deleted' });
    } else if (saved.config_revision !== previous.configRevision) {
      dispatchCompare({
        type: 'job_execution_expired',
        jobId: previous.jobId,
        configRevision: previous.configRevision,
        reason: 'job_changed',
      });
    }
  }, [dispatchCompare]);

  const refreshLatestRunSummaries = useCallback(() => {
    const ticket = latestRunSummaryFence.current.start('latest-run-summaries');
    ipc.latestRunRecords().then(
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
    _name: string,
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

  const expireDeletedJobState = useCallback((jobId: string) => {
    dispatchCompare({ type: 'job_execution_expired', jobId, reason: 'job_deleted' });
    dispatchRootEditor({ type: 'job_removed', jobId });
  }, [dispatchCompare, dispatchRootEditor]);

  useWorkspaceBootstrap({ refreshJobs, refreshLatestRunSummaries, setJobsDir, setAppVersion, setStatus });

  useEffect(() => {
    if (!selectedJob) { setJobConfiguration(null); return; }
    let live = true;
    ipc.getJob(selectedJob.name).then((detail) => {
      if (live) setJobConfiguration(detail.job);
    }).catch((error) => {
      if (!live) return;
      setJobConfiguration(null);
      setStatus(`Failed to load '${selectedJob.name}' settings: ${error}`, 'err');
    });
    return () => { live = false; };
  }, [selectedJob, setStatus]);

  return {
    appVersion,
    describeMutationFailure,
    expireDeletedJobState,
    jobConfiguration,
    jobsDir,
    latestRunByJobId,
    pathHistory,
    pushHistory,
    reconcileSavedWorkspaceJob,
    reconcileWorkspaceJob,
    refreshLatestRunSummaries,
    setJobConfiguration,
  };
}
