import type { PlanDto } from '../../core/plan';
import type { CompareOwner } from '../../core/types/generated/CompareOwner';
import type { JobDto } from '../../core/types/generated/JobDto';

/// The only retained compare result. Keeping its provenance beside the review vectors prevents a
/// plan from ever being interpreted through whichever job happens to be selected now.
export interface CompareSession {
  plan: PlanDto;
  checked: boolean[];
  flipped: boolean[];
}

export function successfulSession(plan: PlanDto, checked: boolean[], flipped: boolean[]): CompareSession {
  return { plan, checked, flipped };
}

export function ownerMatchesSelection(owner: CompareOwner, job: JobDto | null, targetIndex: number): boolean {
  return !!job
    && owner.job_name === job.name
    && owner.target_index === targetIndex
    && owner.config_revision === job.config_revision;
}

export function activeSession(
  session: CompareSession | null,
  job: JobDto | null,
  targetIndex: number,
): CompareSession | null {
  return session && ownerMatchesSelection(session.plan.owner, job, targetIndex) ? session : null;
}

/// Returning to the job that owns the one retained result also returns to that result's target.
/// A changed config revision makes the slot ineligible even when the job name stayed the same.
export function targetForSelection(session: CompareSession | null, job: JobDto): number {
  if (!session || session.plan.owner.job_name !== job.name || session.plan.owner.config_revision !== job.config_revision) return 0;
  const index = session.plan.owner.target_index;
  return index >= 0 && index < job.targets.length ? index : 0;
}

export function invalidateJobSession(session: CompareSession | null, jobName: string): CompareSession | null {
  return session?.plan.owner.job_name === jobName ? null : session;
}

/// A successful job-list refresh is authoritative. A changed revision keeps the bounded slot but
/// makes it inactive through ownerMatchesSelection; a missing job retires its slot so recreating a
/// file with the same name cannot resurrect an orphaned result.
export function reconcileRefreshedJobSession(
  session: CompareSession | null,
  jobName: string,
  refreshedJob: JobDto | null,
): CompareSession | null {
  return refreshedJob ? session : invalidateJobSession(session, jobName);
}

/// Saving an effectively identical job is not invalidation. The backend returns the canonical
/// revision it wrote, so this decision does not depend on a second, fallible list refresh.
export function reconcileSavedJobSession(
  session: CompareSession | null,
  jobName: string,
  configRevision: string,
): CompareSession | null {
  if (session?.plan.owner.job_name !== jobName) return session;
  return session.plan.owner.config_revision === configRevision ? session : null;
}
