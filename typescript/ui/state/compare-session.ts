import type { PlanDto } from '../../core/plan';
import type { CompareOwner } from '../../core/types/generated/CompareOwner';
import type { JobDto } from '../../core/types/generated/JobDto';
import type { JobSaveDto } from '../../core/types/generated/JobSaveDto';

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
  return left.identity.job_id === right.identity.job_id
    && left.identity.target_index === right.identity.target_index
    && left.identity.config_revision === right.identity.config_revision;
}

function sameIdentity(left: CompareOwner, right: CompareOwner): boolean {
  return sameKey(left, right)
    && left.identity.compare_run_id === right.identity.compare_run_id;
}

export interface JobIdentitySnapshot {
  jobId: string;
  name: string;
  configRevision: string;
}

export function compareScopeKey(jobId: string, targetIndex: number, configRevision: string): string {
  return `${jobId}\0${targetIndex}\0${configRevision}`;
}

export function snapshotJob(job: JobDto): JobIdentitySnapshot {
  return { jobId: job.job_id, name: job.name, configRevision: job.config_revision };
}

function withOwnerName(session: CompareSession, jobName: string): CompareSession {
  if (session.plan.owner.job_name === jobName) return session;
  return {
    ...session,
    plan: { ...session.plan, owner: { ...session.plan.owner, job_name: jobName } },
  };
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

export function retainRestoredSession(
  repository: CompareRepository,
  session: CompareSession,
): CompareRepository {
  if (repository.sessions.some((candidate) => sameKey(candidate.plan.owner, session.plan.owner))) {
    return repository;
  }
  return retainSuccessfulSession(repository, session);
}

export function ownerMatchesSelection(owner: CompareOwner, job: JobDto | null, targetIndex: number): boolean {
  return !!job
    && owner.identity.job_id === job.job_id
    && owner.identity.target_index === targetIndex
    && owner.identity.config_revision === job.config_revision;
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
    candidate.plan.owner.identity.job_id === job.job_id
    && candidate.plan.owner.identity.config_revision === job.config_revision
    && candidate.plan.owner.identity.target_index >= 0
    && candidate.plan.owner.identity.target_index < job.targets.length
  ));
  return session?.plan.owner.identity.target_index ?? 0;
}

export function invalidateJobRevision(
  repository: CompareRepository,
  jobId: string,
  configRevision: string,
): CompareRepository {
  const sessions = repository.sessions.filter((session) => (
    session.plan.owner.identity.job_id !== jobId
    || session.plan.owner.identity.config_revision !== configRevision
  ));
  return sessions.length === repository.sessions.length ? repository : { sessions };
}

export function invalidateSession(
  repository: CompareRepository,
  owner: CompareOwner,
): CompareRepository {
  const sessions = repository.sessions.filter((session) => !sameIdentity(session.plan.owner, owner));
  return sessions.length === repository.sessions.length ? repository : { sessions };
}

export function invalidateJobSession(repository: CompareRepository, jobId: string): CompareRepository {
  const sessions = repository.sessions.filter((session) => session.plan.owner.identity.job_id !== jobId);
  return sessions.length === repository.sessions.length ? repository : { sessions };
}

export function reconcileRefreshedJobSession(
  repository: CompareRepository,
  previous: JobIdentitySnapshot,
  refreshedJob: JobDto | null,
): CompareRepository {
  if (!refreshedJob || refreshedJob.job_id !== previous.jobId) {
    return invalidateJobSession(repository, previous.jobId);
  }
  let changed = false;
  const sessions: CompareSession[] = [];
  for (const session of repository.sessions) {
    if (session.plan.owner.identity.job_id !== previous.jobId) {
      sessions.push(session);
      continue;
    }
    if (session.plan.owner.identity.config_revision !== refreshedJob.config_revision) {
      changed = true;
      continue;
    }
    const rebound = withOwnerName(session, refreshedJob.name);
    if (rebound !== session) changed = true;
    sessions.push(rebound);
  }
  return changed ? { sessions } : repository;
}

export function reconcileSavedJobSession(
  repository: CompareRepository,
  saved: JobSaveDto,
  original: JobIdentitySnapshot | null,
): CompareRepository {
  if (!original || saved.effect === 'created' || saved.effect === 'no_op') return repository;
  if (saved.job_id !== original.jobId) return invalidateJobSession(repository, original.jobId);

  let changed = false;
  const sessions: CompareSession[] = [];
  for (const session of repository.sessions) {
    if (session.plan.owner.identity.job_id !== original.jobId) {
      sessions.push(session);
      continue;
    }
    if (saved.config_revision !== original.configRevision
      && session.plan.owner.identity.config_revision === original.configRevision) {
      changed = true;
      continue;
    }
    const rebound = withOwnerName(session, saved.name);
    if (rebound !== session) changed = true;
    sessions.push(rebound);
  }
  return changed ? { sessions } : repository;
}

/// Reinsert or refresh one exact session only after the backend confirms that result is retained.
/// A delayed confirmation never replaces a newer result already published for the same scope.
export function retainConfirmedSession(
  repository: CompareRepository,
  retainedSession: CompareSession,
  currentOwner: CompareOwner,
): CompareRepository {
  if (!sameIdentity(retainedSession.plan.owner, currentOwner)) return repository;
  const scopeIndex = repository.sessions.findIndex((session) => sameKey(session.plan.owner, currentOwner));
  if (scopeIndex >= 0
    && !sameIdentity(repository.sessions[scopeIndex].plan.owner, currentOwner)) return repository;
  const currentSession = scopeIndex >= 0 ? repository.sessions[scopeIndex] : retainedSession;
  const confirmed = currentSession.plan.owner.job_name === currentOwner.job_name
    ? currentSession
    : { ...currentSession, plan: { ...currentSession.plan, owner: currentOwner } };
  if (scopeIndex < 0) {
    return { sessions: [confirmed, ...repository.sessions].slice(0, COMPARE_SESSION_CAPACITY) };
  }
  if (scopeIndex === 0 && repository.sessions[0] === confirmed) return repository;
  const sessions = [...repository.sessions];
  sessions.splice(scopeIndex, 1);
  sessions.unshift(confirmed);
  return { sessions };
}
