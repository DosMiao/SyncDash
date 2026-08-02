// How a scope's workspace is created, promoted and replaced inside the repository index.
// A replaced workspace keeps the retained evidence of results the user has not dismissed; losing
// it would silently drop a reviewed plan.

// Repository transitions own cross-result ordering, publication, retention, and async request
// fencing. Per-result review transitions stay independent of these lifecycle rules.
import type { CompareScopeExecutionStatusDto } from '#core/types/generated/CompareScopeExecutionStatusDto.ts';
import type { CompareWorkspaceLookupDto } from '#core/types/generated/CompareWorkspaceLookupDto.ts';
import type { CompareWorkspaceSnapshotDto } from '#core/types/generated/CompareWorkspaceSnapshotDto.ts';
import type { CompareScopeDto } from '#core/types/generated/CompareScopeDto.ts';
import {
  compareResultKey,
  compareScopeFromIdentity,
  compareScopeKey,
  createCompareWorkspace,
  defaultCompareWorkspacePreferences,
  sameCompareScope,
} from '../compareWorkspaceModel.ts';
import type {
  CompareActivityOrigin,
  CompareResultKey,
  CompareScopeKey,
  CompareScopeWorkspace,
  CompareWorkspace,
  CompareWorkspacePreferences,
  CompareWorkspaceRepository,
  ScopeRestorationState,
  StagedCompareCandidate,
} from '../compareWorkspaceModel.ts';
import {
  deriveWorkspaceExecutionAccess,
  isConsistentCompareExecutionStatus,
  isConsistentComparePublication,
  isConsistentCompareWorkspaceSnapshot,
  reconcileExecutionStatus,
} from '../compareWorkspaceExecution.ts';
type CompareWorkspaceRepositoryAction =
  | { type: 'scope_touched'; scope: CompareScopeDto }
  | { type: 'execution_status_received'; execution: CompareScopeExecutionStatusDto }
  | { type: 'scope_restore_started'; scope: CompareScopeDto; requestId: number }
  | {
    type: 'scope_restore_completed';
    scopeKey: CompareScopeKey;
    requestId: number;
    lookup: CompareWorkspaceLookupDto;
    preferences?: CompareWorkspacePreferences;
  }
  | { type: 'scope_restore_failed'; scopeKey: CompareScopeKey; requestId: number; error: string }
  | {
    type: 'manual_compare_published';
    snapshot: CompareWorkspaceSnapshotDto;
    preferences?: CompareWorkspacePreferences;
  }
  | {
    type: 'autoscan_compare_published';
    snapshot: CompareWorkspaceSnapshotDto;
    generation: number;
    ticketId: number;
    preferences?: CompareWorkspacePreferences;
  }
  | { type: 'candidate_adopted'; scopeKey: CompareScopeKey; expectedResultKey: CompareResultKey }
  | { type: 'candidate_discarded'; scopeKey: CompareScopeKey; expectedResultKey: CompareResultKey }
  | { type: 'workspace_lookup_started'; workspace: CompareWorkspace; requestId: number }
  | {
    type: 'workspace_lookup_completed';
    resultKey: CompareResultKey;
    requestId: number;
    lookup: CompareWorkspaceLookupDto;
  }
  | { type: 'workspace_lookup_failed'; resultKey: CompareResultKey; requestId: number; error: string }
  | {
    type: 'compare_activity_started';
    scope: CompareScopeDto;
    requestId: number;
    origin: CompareActivityOrigin;
  }
  | { type: 'compare_activity_finished'; scopeKey: CompareScopeKey; requestId: number }
  | { type: 'compare_activity_failed'; scopeKey: CompareScopeKey; requestId: number; error: string }
  | {
    type: 'job_execution_expired';
    jobId: string;
    configRevision: string;
    reason: 'job_changed';
  }
  | {
    type: 'job_execution_expired';
    jobId: string;
    configRevision?: never;
    reason: 'job_deleted';
  };

export function createScopeWorkspace(scope: CompareScopeDto): CompareScopeWorkspace {
  return {
    key: compareScopeKey(scope),
    scope,
    execution: null,
    active: null,
    candidate: null,
    dismissedCandidateKey: null,
    latestAutoScanPublication: null,
    activity: { status: 'idle', requestId: 0 },
    restoration: { status: 'idle', requestId: 0 },
  };
}

export function promoteScope(
  repository: CompareWorkspaceRepository,
  scope: CompareScopeWorkspace,
): CompareWorkspaceRepository {
  return {
    scopes: [scope, ...repository.scopes.filter((candidate) => candidate.key !== scope.key)],
  };
}

export function replaceScope(
  repository: CompareWorkspaceRepository,
  key: CompareScopeKey,
  replace: (scope: CompareScopeWorkspace) => CompareScopeWorkspace,
): CompareWorkspaceRepository {
  const index = repository.scopes.findIndex((scope) => scope.key === key);
  if (index < 0) return repository;
  const current = repository.scopes[index];
  const next = replace(current);
  if (next === current) return repository;
  const scopes = [...repository.scopes];
  scopes[index] = next;
  return { scopes };
}

export function replaceExactWorkspace(
  repository: CompareWorkspaceRepository,
  resultKey: CompareResultKey,
  replace: (workspace: CompareWorkspace) => CompareWorkspace,
): CompareWorkspaceRepository {
  for (let index = 0; index < repository.scopes.length; index++) {
    const scope = repository.scopes[index];
    if (scope.active?.key === resultKey) {
      const active = replace(scope.active);
      if (active === scope.active) return repository;
      const scopes = [...repository.scopes];
      scopes[index] = { ...scope, active };
      return { scopes };
    }
    if (scope.candidate?.workspace.key === resultKey) {
      const workspace = replace(scope.candidate.workspace);
      if (workspace === scope.candidate.workspace) return repository;
      const scopes = [...repository.scopes];
      scopes[index] = { ...scope, candidate: { ...scope.candidate, workspace } };
      return { scopes };
    }
  }
  return repository;
}

export function replaceExecutableActiveWorkspace(
  repository: CompareWorkspaceRepository,
  resultKey: CompareResultKey,
  replace: (workspace: CompareWorkspace) => CompareWorkspace,
): CompareWorkspaceRepository {
  const scope = repository.scopes.find((candidate) => candidate.active?.key === resultKey);
  if (!scope
    || !scope.active
    || scope.activity.status !== 'idle'
    || deriveWorkspaceExecutionAccess(scope.active, scope.execution).status !== 'executable') return repository;
  const active = replace(scope.active);
  return active === scope.active
    ? repository
    : replaceScope(repository, scope.key, (current) => ({ ...current, active }));
}
