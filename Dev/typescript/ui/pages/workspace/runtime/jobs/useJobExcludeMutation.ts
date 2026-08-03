import type { JobDetailDto } from '#core/types/generated/JobDetailDto.ts';
import type { JobSaveDto } from '#core/types/generated/JobSaveDto.ts';
import { useCallback } from 'react';
import * as jobsIpc from '#core/infrastructure/tauri/commands/jobs.ts';
import { addExcludeEntries } from '#core/domain/jobs/junk.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import { statusDeliveryWarning, type JobIdentitySnapshot } from '../../model/workspacePageModel.ts';
import type { JobListRefreshOutcome } from './useWorkspaceJobState.ts';

interface JobExcludeMutationOptions {
  selectedJob: JobDto | null;
  describeMutationFailure: (action: string, error: unknown) => Promise<string>;
  reconcileSavedWorkspaceJob: (saved: JobSaveDto, previous: JobIdentitySnapshot | null) => void;
  refreshJobsForAnnouncement: () => Promise<JobListRefreshOutcome>;
  resetSafetyUi: () => void;
  setStatus: StatusApi['setMessage'];
  offerStatusAction: StatusApi['offerAction'];
}

export function useJobExcludeMutation({
  selectedJob,
  describeMutationFailure,
  reconcileSavedWorkspaceJob,
  refreshJobsForAnnouncement,
  resetSafetyUi,
  setStatus,
  offerStatusAction,
}: JobExcludeMutationOptions) {
  return useCallback(async (masks: string[], label: string) => {
    if (!selectedJob) { setStatus('Select a job first', 'err'); return; }
    const name = selectedJob.name;
    let detail: JobDetailDto;
    try {
      detail = await jobsIpc.getJob(name);
    } catch (error) {
      setStatus(await describeMutationFailure(
        'Failed to read the job before adding the exclude',
        error,
      ), 'err');
      return;
    }

    const jobConfiguration = detail.job;
    const { next: nextExcludes, added: addedMasks } = addExcludeEntries(jobConfiguration.exclude, masks);
    if (!addedMasks.length) {
      setStatus(`The job already has ${masks.length > 1 ? 'all of these masks' : 'this exclude'}`);
      return;
    }

    const updatedJob = { ...jobConfiguration, exclude: nextExcludes };
    let saved: JobSaveDto;
    try {
      saved = await jobsIpc.saveJob(name, updatedJob, {
        originalName: detail.name,
        expectedRevision: detail.config_revision,
      });
    } catch (error) {
      setStatus(await describeMutationFailure('Failed to write exclude', error), 'err');
      return;
    }
    reconcileSavedWorkspaceJob(
      saved,
      { jobId: detail.job_id, name: detail.name, configRevision: detail.config_revision },
    );
    resetSafetyUi();

    const undo = async () => {
      let restored: JobSaveDto;
      try {
        restored = await jobsIpc.saveJob(saved.name, jobConfiguration, {
          originalName: saved.name,
          expectedRevision: saved.config_revision,
        });
        reconcileSavedWorkspaceJob(
          restored,
          { jobId: saved.job_id, name: saved.name, configRevision: saved.config_revision },
        );
      } catch (error) {
        throw new Error(await describeMutationFailure('Could not undo the exclude', error));
      }
      resetSafetyUi();
      const undoneWarning = statusDeliveryWarning(restored);
      const undoneRefresh = await refreshJobsForAnnouncement();
      setStatus(
        `Exclude undone${undoneWarning}${undoneRefresh.suffix}`,
        undoneWarning || undoneRefresh.failed ? 'err' : '',
      );
    };

    const warning = statusDeliveryWarning(saved);
    const success = `${label}: ${addedMasks.join(', ')} — Compare again to build a result with this exclusion${warning}`;
    const refresh = await refreshJobsForAnnouncement();
    offerStatusAction(
      `${success}${refresh.suffix}`,
      'Undo exclude',
      undo,
      warning || refresh.failed ? 'err' : '',
    );
  }, [
    describeMutationFailure,
    offerStatusAction,
    reconcileSavedWorkspaceJob,
    refreshJobsForAnnouncement,
    resetSafetyUi,
    selectedJob,
    setStatus,
  ]);
}
