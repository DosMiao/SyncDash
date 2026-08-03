import { useCallback } from 'react';
import type { Dispatch, SetStateAction } from 'react';
import type { JobEditorIdentity } from '#ui/features/jobs/model/jobEditorModel.ts';
import type { Job as JobFull } from '#core/types/generated/Job.ts';
import type { JobDeleteDto } from '#core/types/generated/JobDeleteDto.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { JobSaveDto } from '#core/types/generated/JobSaveDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import { statusDeliveryWarning } from '../../model/workspacePageModel.ts';
import type { JobEditorIntent } from '../interaction/useWorkspacePanels.ts';
import type { JobListRefreshOutcome } from './useWorkspaceJobState.ts';

interface JobEditorLifecycleOptions {
  selectedJob: JobDto | null;
  setEditor: Dispatch<SetStateAction<JobEditorIntent | null>>;
  reconcileSavedWorkspaceJob: (saved: JobSaveDto, previous: JobEditorIdentity | null) => void;
  reconcileWorkspaceJob: (previous: JobEditorIdentity, refreshedJob: JobDto | null) => void;
  expireDeletedJobState: (jobId: string) => void;
  resetSafetyUi: () => void;
  setRegistrySelection: (job: JobDto | null) => void;
  setSelectedTargetIndex: Dispatch<SetStateAction<number>>;
  refreshJobs: () => Promise<JobDto[]>;
  refreshJobsForAnnouncement: () => Promise<JobListRefreshOutcome>;
  pushHistory: (path: string) => void;
  setStatus: StatusApi['setMessage'];
}

/** Reconciles editor mutations with registry selection, retained evidence, and path history. */
export function useJobEditorLifecycle({
  selectedJob,
  setEditor,
  reconcileSavedWorkspaceJob,
  reconcileWorkspaceJob,
  expireDeletedJobState,
  resetSafetyUi,
  setRegistrySelection,
  setSelectedTargetIndex,
  refreshJobs,
  refreshJobsForAnnouncement,
  pushHistory,
  setStatus,
}: JobEditorLifecycleOptions) {
  const onSaved = useCallback(async (
    saved: JobSaveDto,
    job: JobFull,
    original: JobEditorIdentity | null,
  ) => {
    const selectedIdentity = !!original && selectedJob?.job_id === original.jobId;
    const semanticMutation = !!original && saved.config_revision !== original.configRevision;
    setEditor(null);
    reconcileSavedWorkspaceJob(saved, original);
    if (selectedIdentity && semanticMutation) resetSafetyUi();
    pushHistory(job.source);
    for (const target of job.targets) pushHistory(target);
    const warning = statusDeliveryWarning(saved);
    const refresh = await refreshJobsForAnnouncement();
    if (refresh.failed) {
      setStatus(`Saved '${saved.name}'${warning}${refresh.suffix}`, 'err');
      return;
    }
    setStatus(
      (saved.effect === 'no_op' ? `No changes to save for '${saved.name}'` : `Saved '${saved.name}'`) + warning,
      warning ? 'err' : 'ok',
    );
  }, [
    pushHistory,
    reconcileSavedWorkspaceJob,
    refreshJobsForAnnouncement,
    resetSafetyUi,
    selectedJob?.job_id,
    setEditor,
    setStatus,
  ]);

  const onDeleted = useCallback(async (deleted: JobDeleteDto) => {
    setEditor(null);
    expireDeletedJobState(deleted.job_id);
    if (selectedJob?.job_id === deleted.job_id) {
      resetSafetyUi();
      setRegistrySelection(null);
      setSelectedTargetIndex(0);
    }
    const warning = statusDeliveryWarning(deleted);
    const refresh = await refreshJobsForAnnouncement();
    setStatus(
      `Deleted '${deleted.name}'${warning}${refresh.suffix}`,
      warning || refresh.failed ? 'err' : '',
    );
  }, [
    expireDeletedJobState,
    refreshJobsForAnnouncement,
    resetSafetyUi,
    selectedJob?.job_id,
    setEditor,
    setRegistrySelection,
    setSelectedTargetIndex,
    setStatus,
  ]);

  const onMutationConflict = useCallback(async (
    _name: string,
    original: JobEditorIdentity | null,
  ) => {
    const list = await refreshJobs();
    if (!original) return;
    const refreshed = list.find((candidate) => candidate.job_id === original.jobId) ?? null;
    reconcileWorkspaceJob(original, refreshed);
    if (selectedJob?.job_id === original.jobId
      && (!refreshed || selectedJob.config_revision !== refreshed.config_revision)) resetSafetyUi();
  }, [reconcileWorkspaceJob, refreshJobs, resetSafetyUi, selectedJob]);

  return { onDeleted, onMutationConflict, onSaved };
}
