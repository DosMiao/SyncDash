import { useCallback, useMemo, useReducer, useRef } from 'react';

import { listJobs } from '#core/infrastructure/tauri/commands/jobs.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import {
  emptyJobRegistryState,
  reduceJobRegistry,
  validateJobRegistrySnapshot,
} from '#core/application/jobs/jobRegistry.ts';
import { SerialRequestQueue } from '#core/application/coordination/serialRequestQueue.ts';

export interface JobRegistryController {
  jobs: JobDto[];
  selectedJob: JobDto | null;
  refresh: () => Promise<JobDto[]>;
  select: (job: JobDto | null) => void;
}

export function useJobRegistryController(): JobRegistryController {
  const [state, dispatch] = useReducer(reduceJobRegistry, emptyJobRegistryState);
  const refreshQueue = useRef(new SerialRequestQueue());

  const refresh = useCallback(async () => {
    const jobs = await refreshQueue.current.run(listJobs);
    validateJobRegistrySnapshot(jobs);
    dispatch({ type: 'snapshot_received', jobs });
    return jobs;
  }, []);

  const select = useCallback((job: JobDto | null) => {
    dispatch({ type: 'selection_changed', jobId: job?.job_id ?? null });
  }, []);

  const selectedJob = useMemo(() => (
    state.jobs.find((job) => job.job_id === state.selectedJobId) ?? null
  ), [state.jobs, state.selectedJobId]);

  return {
    jobs: state.jobs,
    selectedJob,
    refresh,
    select,
  };
}
