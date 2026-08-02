import { JOB_FORM_FIELDS, formGroupNames } from '#core/domain/jobs/formSchema.ts';
import type { FormValues } from '#core/domain/jobs/formSchema.ts';
import type { Job as JobFull } from '#core/types/generated/Job.ts';

export interface JobEditorApi {
  setField: (key: string, value: string) => void;
}

export interface JobEditorIdentity {
  jobId: string;
  name: string;
  configRevision: string;
}

/** `base` retains fields outside the visible schema so a save cannot reset them. */
export interface LoadedJobForm {
  base: JobFull;
  values: FormValues;
  initialValues: FormValues;
  originalName: string | null;
  jobId: string | null;
  configRevision: string | null;
}

export interface SaveError {
  message: string;
  field?: string;
}

export type MutationKind = 'save' | 'delete';

export const JOB_FORM_GROUPS = formGroupNames(JOB_FORM_FIELDS);

export function sameFormValues(left: FormValues, right: FormValues): boolean {
  const keys = new Set([...Object.keys(left), ...Object.keys(right)]);
  return [...keys].every((key) => left[key] === right[key]);
}
