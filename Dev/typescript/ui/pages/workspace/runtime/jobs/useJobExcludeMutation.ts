import type { JobDetailDto } from '#core/types/generated/JobDetailDto.ts';
import type { JobSaveDto } from '#core/types/generated/JobSaveDto.ts';
import { useCallback } from 'react';
import * as jobsIpc from '#core/infrastructure/tauri/commands/jobs.ts';
import { addExcludeEntries } from '#core/domain/jobs/junk.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import { statusDeliveryWarning, type JobIdentitySnapshot } from '../../model/workspacePageModel.ts';

interface JobExcludeMutationOptions {
  selectedJob: JobDto | null;
  describeMutationFailure: (name: string, action: string, error: unknown) => Promise<string>;
  reconcileSavedWorkspaceJob: (saved: JobSaveDto, previous: JobIdentitySnapshot | null) => void;
  refreshJobs: () => Promise<JobDto[]>;
  resetSafetyUi: () => void;
  setStatus: StatusApi['setMessage'];
  offerStatusAction: StatusApi['offerAction'];
}

export function useJobExcludeMutation({
  selectedJob,
  describeMutationFailure,
  reconcileSavedWorkspaceJob,
  refreshJobs,
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
        name,
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
      setStatus(await describeMutationFailure(name, 'Failed to write exclude', error), 'err');
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
        throw new Error(await describeMutationFailure(saved.name, 'Could not undo the exclude', error));
      }
      resetSafetyUi();
      const warning = statusDeliveryWarning(restored);
      try {
        await refreshJobs();
        setStatus(`Exclude undone${warning}`, warning ? 'err' : '');
      } catch (error) {
        setStatus(`Exclude undone${warning} · job-list refresh failed: ${error}`, 'err');
      }
    };

    const warning = statusDeliveryWarning(saved);
    const success = `${label}: ${addedMasks.join(', ')} — Compare again to build a result with this exclusion${warning}`;
    try {
      await refreshJobs();
      offerStatusAction(success, 'Undo exclude', undo, warning ? 'err' : '');
    } catch (error) {
      offerStatusAction(`${success} · job-list refresh failed: ${error}`, 'Undo exclude', undo, 'err');
    }
  }, [
    describeMutationFailure,
    offerStatusAction,
    reconcileSavedWorkspaceJob,
    refreshJobs,
    resetSafetyUi,
    selectedJob,
    setStatus,
  ]);
}
