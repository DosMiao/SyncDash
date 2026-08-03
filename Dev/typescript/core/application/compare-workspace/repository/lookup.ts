// Resolving a stored result by exact identity, and saying precisely why a lookup failed.
// The problem functions are exported because the UI must distinguish "expired", "superseded" and
// "never existed" — collapsing them would hide whether re-running Compare helps.

import type { CompareScopeDto } from '#core/types/generated/CompareScopeDto.ts';
import type { CompareWorkspaceLookupDto } from '#core/types/generated/CompareWorkspaceLookupDto.ts';
import {
  compareResultKey,
  compareScopeFromIdentity,
  compareScopeKey,
  createCompareWorkspace,
  sameCompareScope,
} from '../compareWorkspaceModel.ts';
import type {
  CompareResultKey,
  CompareWorkspace,
  CompareWorkspaceRepository,
} from '../compareWorkspaceModel.ts';
import {
  isConsistentCompareExecutionStatus,
  isConsistentCompareWorkspaceSnapshot,
  reconcileExecutionStatus,
} from '../compareWorkspaceExecution.ts';
import { createScopeWorkspace, promoteScope } from './scopeIndex.ts';
import { refreshPublishedWorkspace } from './lifecycle.ts';

export function beginExactWorkspaceLookup(
  repository: CompareWorkspaceRepository,
  capturedWorkspace: CompareWorkspace,
  requestId: number,
): CompareWorkspaceRepository {
  const resultKey = capturedWorkspace.key;
  const existingScope = repository.scopes.find((scope) => (
    scope.active?.key === resultKey || scope.candidate?.workspace.key === resultKey
  ));
  if (existingScope) {
    const workspace = existingScope.active?.key === resultKey
      ? existingScope.active
      : existingScope.candidate!.workspace;
    if (requestId <= workspace.retention.requestId) return promoteScope(repository, existingScope);
    const checking = { ...workspace, retention: { status: 'checking', requestId } as const };
    return promoteScope(repository, existingScope.active?.key === resultKey
      ? { ...existingScope, active: checking }
      : {
        ...existingScope,
        candidate: { ...existingScope.candidate!, workspace: checking },
      });
  }

  if (requestId <= capturedWorkspace.retention.requestId) return repository;
  const scopeIdentity = compareScopeFromIdentity(capturedWorkspace.identity);
  const scopeKeyValue = compareScopeKey(scopeIdentity);
  const sameScope = repository.scopes.find((scope) => scope.key === scopeKeyValue);
  if (sameScope?.active || sameScope?.candidate) return repository;
  const restored = {
    ...capturedWorkspace,
    retention: { status: 'checking', requestId } as const,
  };
  return promoteScope(repository, {
    ...(sameScope ?? createScopeWorkspace(scopeIdentity)),
    active: restored,
  });
}

export function exactWorkspaceLookupProblem(
  expectedScope: CompareScopeDto,
  resultKey: CompareResultKey,
  lookup: CompareWorkspaceLookupDto,
): string | null {
  if (lookup.status === 'missing') {
    if (!isConsistentCompareExecutionStatus(lookup.execution_status)) {
      return 'The backend returned an internally inconsistent Compare execution status';
    }
    return sameCompareScope(expectedScope, lookup.execution_status.scope)
      ? null
      : 'The backend returned a missing result for a different Compare scope';
  }
  const snapshot = lookup.workspace;
  if (!isConsistentCompareWorkspaceSnapshot(snapshot)) {
    return 'The backend returned an internally inconsistent Compare workspace';
  }
  if (compareResultKey(snapshot.plan.owner.identity) !== resultKey) {
    return 'The backend returned a different exact Compare result';
  }
  return sameCompareScope(expectedScope, compareScopeFromIdentity(snapshot.plan.owner.identity))
    ? null
    : 'The backend returned a Compare workspace for a different scope';
}

export function scopeWorkspaceLookupProblem(
  expectedScope: CompareScopeDto,
  lookup: CompareWorkspaceLookupDto,
): string | null {
  if (lookup.status === 'missing') {
    if (!isConsistentCompareExecutionStatus(lookup.execution_status)) {
      return 'The backend returned an internally inconsistent Compare execution status';
    }
    return sameCompareScope(expectedScope, lookup.execution_status.scope)
      ? null
      : 'The backend answered this restore request for a different Compare scope';
  }
  if (!isConsistentCompareWorkspaceSnapshot(lookup.workspace)) {
    return 'The backend returned an internally inconsistent Compare workspace';
  }
  return sameCompareScope(expectedScope, compareScopeFromIdentity(lookup.workspace.plan.owner.identity))
    && sameCompareScope(expectedScope, lookup.workspace.execution_status.scope)
    ? null
    : 'The backend returned a retained result for a different Compare scope';
}

export function completeExactWorkspaceLookup(
  repository: CompareWorkspaceRepository,
  resultKey: CompareResultKey,
  requestId: number,
  lookup: CompareWorkspaceLookupDto,
): CompareWorkspaceRepository {
  for (let index = 0; index < repository.scopes.length; index++) {
    const scope = repository.scopes[index];
    const activeMatches = scope.active?.key === resultKey;
    const candidateMatches = scope.candidate?.workspace.key === resultKey;
    const workspace = activeMatches
      ? scope.active
      : candidateMatches
        ? scope.candidate!.workspace
        : null;
    if (!workspace
      || workspace.retention.status !== 'checking'
      || workspace.retention.requestId !== requestId) continue;

    const lookupProblem = exactWorkspaceLookupProblem(scope.scope, resultKey, lookup);
    let refreshed: CompareWorkspace;
    let execution = scope.execution;
    if (lookupProblem) {
      refreshed = {
        ...workspace,
        retention: { status: 'check_failed', requestId, error: lookupProblem },
      };
    } else if (lookup.status === 'missing') {
      refreshed = { ...workspace, retention: { status: 'missing', requestId } };
      execution = reconcileExecutionStatus(scope.execution, lookup.execution_status);
    } else {
      const snapshot = lookup.workspace;
      refreshed = refreshPublishedWorkspace(
        workspace,
        createCompareWorkspace(snapshot.plan),
        requestId,
      );
      execution = reconcileExecutionStatus(scope.execution, snapshot.execution_status);
    }

    const updatedScope = activeMatches
      ? { ...scope, active: refreshed, execution }
      : {
        ...scope,
        candidate: { ...scope.candidate!, workspace: refreshed },
        execution,
      };
    const scopes = [...repository.scopes];
    scopes[index] = updatedScope;
    return { scopes };
  }
  return repository;
}
