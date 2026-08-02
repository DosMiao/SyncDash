import { invoke } from '@tauri-apps/api/core';

import type { Job as JobFull } from '#core/types/generated/Job.ts';
import type { JobDeleteDto } from '#core/types/generated/JobDeleteDto.ts';
import type { JobDetailDto } from '#core/types/generated/JobDetailDto.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { JobFileSchemaDto } from '#core/types/generated/JobFileSchemaDto.ts';
import type { JobRootField } from '#core/types/generated/JobRootField.ts';
import type { JobRootMutationDto } from '#core/types/generated/JobRootMutationDto.ts';
import type { JobSaveDto } from '#core/types/generated/JobSaveDto.ts';

export type {
  JobDeleteDto,
  JobDetailDto,
  JobFull,
  JobRootField,
  JobRootMutationDto,
  JobSaveDto,
};

export const listJobs = () => invoke<JobDto[]>('list_jobs');
export const getJob = (name: string) => invoke<JobDetailDto>('get_job', { name });

// New-job policy comes from the engine so frontend defaults cannot drift from Job::default().
export const defaultJob = () => invoke<JobFull>('default_job');

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
