import type { JobDto } from '#core/types/generated/JobDto.ts';

export interface JobRegistryState {
  jobs: JobDto[];
  selectedJobId: string | null;
}

export type JobRegistryAction =
  | { type: 'snapshot_received'; jobs: JobDto[] }
  | { type: 'selection_changed'; jobId: string | null };

export const emptyJobRegistryState: JobRegistryState = {
  jobs: [],
  selectedJobId: null,
};

export function validateJobRegistrySnapshot(jobs: JobDto[]): void {
  const identities = new Set<string>();
  const names = new Set<string>();
  for (const job of jobs) {
    if (!job.job_id) throw new Error(`The job registry returned '${job.name}' without an identity`);
    if (identities.has(job.job_id)) {
      throw new Error(`The job registry returned duplicate identity '${job.job_id}'`);
    }
    if (names.has(job.name)) {
      throw new Error(`The job registry returned duplicate name '${job.name}'`);
    }
    identities.add(job.job_id);
    names.add(job.name);
  }
}

export function reduceJobRegistry(
  state: JobRegistryState,
  action: JobRegistryAction,
): JobRegistryState {
  switch (action.type) {
    case 'snapshot_received':
      return {
        jobs: action.jobs,
        selectedJobId: state.selectedJobId !== null
          && action.jobs.some((job) => job.job_id === state.selectedJobId)
          ? state.selectedJobId
          : null,
      };
    case 'selection_changed':
      return {
        ...state,
        selectedJobId: action.jobId !== null
          && state.jobs.some((job) => job.job_id === action.jobId)
          ? action.jobId
          : null,
      };
  }
}
