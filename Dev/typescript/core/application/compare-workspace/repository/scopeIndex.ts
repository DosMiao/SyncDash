// How a scope's workspace is created, promoted and replaced inside the repository index.
// A replaced workspace keeps the retained evidence of results the user has not dismissed; losing
// it would silently drop a reviewed plan.

import type { CompareScopeDto } from '#core/types/generated/CompareScopeDto.ts';
import { compareScopeKey } from '../compareWorkspaceModel.ts';
import type {
  CompareResultKey,
  CompareScopeKey,
  CompareScopeWorkspace,
  CompareWorkspace,
  CompareWorkspaceRepository,
} from '../compareWorkspaceModel.ts';
import { deriveWorkspaceExecutionAccess } from '../compareWorkspaceExecution.ts';

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
