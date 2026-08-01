import type { PlanDto } from '../../core/plan';
import type { CompareOwner } from '../../core/types/generated/CompareOwner';
import type { JobDto } from '../../core/types/generated/JobDto';

export const COMPARE_SESSION_CAPACITY = 8;

export interface CompareSession {
  plan: PlanDto;
  checked: boolean[];
  flipped: boolean[];
}

export interface CompareRepository {
  sessions: CompareSession[];
}

export const EMPTY_COMPARE_REPOSITORY: CompareRepository = { sessions: [] };

export function successfulSession(plan: PlanDto, checked: boolean[], flipped: boolean[]): CompareSession {
  return { plan, checked, flipped };
}

function sameKey(left: CompareOwner, right: CompareOwner): boolean {
  return left.job_name === right.job_name
    && left.target_index === right.target_index
    && left.config_revision === right.config_revision;
}

export function retainSuccessfulSession(
  repository: CompareRepository,
  session: CompareSession,
): CompareRepository {
  const sessions = [
    session,
    ...repository.sessions.filter((candidate) => !sameKey(candidate.plan.owner, session.plan.owner)),
  ].slice(0, COMPARE_SESSION_CAPACITY);
  return { sessions };
}

export function ownerMatchesSelection(owner: CompareOwner, job: JobDto | null, targetIndex: number): boolean {
  return !!job
    && owner.job_name === job.name
    && owner.target_index === targetIndex
    && owner.config_revision === job.config_revision;
}

export function activeSession(
  repository: CompareRepository,
  job: JobDto | null,
  targetIndex: number,
): CompareSession | null {
  return repository.sessions.find((session) => ownerMatchesSelection(session.plan.owner, job, targetIndex)) ?? null;
}

export function touchSession(
  repository: CompareRepository,
  job: JobDto | null,
  targetIndex: number,
): CompareRepository {
  const index = repository.sessions.findIndex((session) => ownerMatchesSelection(session.plan.owner, job, targetIndex));
  if (index <= 0) return repository;
  const sessions = [...repository.sessions];
  const [session] = sessions.splice(index, 1);
  sessions.unshift(session);
  return { sessions };
}

export function updateSession(
  repository: CompareRepository,
  job: JobDto | null,
  targetIndex: number,
  update: (session: CompareSession) => CompareSession,
): CompareRepository {
  const index = repository.sessions.findIndex((session) => ownerMatchesSelection(session.plan.owner, job, targetIndex));
  if (index < 0) return repository;
  const sessions = [...repository.sessions];
  sessions[index] = update(sessions[index]);
  return { sessions };
}

export function targetForSelection(repository: CompareRepository, job: JobDto): number {
  const session = repository.sessions.find((candidate) => (
    candidate.plan.owner.job_name === job.name
    && candidate.plan.owner.config_revision === job.config_revision
    && candidate.plan.owner.target_index >= 0
    && candidate.plan.owner.target_index < job.targets.length
  ));
  return session?.plan.owner.target_index ?? 0;
}

export function invalidateJobRevision(
  repository: CompareRepository,
  jobName: string,
  configRevision: string,
): CompareRepository {
  const sessions = repository.sessions.filter((session) => (
    session.plan.owner.job_name !== jobName
    || session.plan.owner.config_revision !== configRevision
  ));
  return sessions.length === repository.sessions.length ? repository : { sessions };
}

export function invalidateSession(
  repository: CompareRepository,
  owner: CompareOwner,
): CompareRepository {
  const sessions = repository.sessions.filter((session) => !sameKey(session.plan.owner, owner));
  return sessions.length === repository.sessions.length ? repository : { sessions };
}

export function invalidateJobSession(repository: CompareRepository, jobName: string): CompareRepository {
  const sessions = repository.sessions.filter((session) => session.plan.owner.job_name !== jobName);
  return sessions.length === repository.sessions.length ? repository : { sessions };
}

export function reconcileRefreshedJobSession(
  repository: CompareRepository,
  jobName: string,
  refreshedJob: JobDto | null,
): CompareRepository {
  if (!refreshedJob) return invalidateJobSession(repository, jobName);
  const sessions = repository.sessions.filter((session) => (
    session.plan.owner.job_name !== jobName
    || session.plan.owner.config_revision === refreshedJob.config_revision
  ));
  return sessions.length === repository.sessions.length ? repository : { sessions };
}

export function reconcileSavedJobSession(
  repository: CompareRepository,
  originalName: string,
  originalRevision: string,
  savedName: string,
  savedRevision: string,
): CompareRepository {
  if (originalName === savedName && originalRevision === savedRevision) return repository;
  if (originalName !== savedName) return invalidateJobSession(repository, originalName);
  return invalidateJobRevision(repository, originalName, originalRevision);
}
