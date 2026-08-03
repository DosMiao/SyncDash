// Explicit removal of one exact result from the workspace index.
// This is the only transition that discards retained evidence, so it also has to remove every
// reference that would outlive it: the execution authority the result owned, the AutoScan
// publication cursor, and any dismissal recorded against it. A surviving pointer to a forgotten
// result would silently re-enter publication and lookup decisions that assume the key resolves.

import type {
  CompareResultKey,
  CompareScopeWorkspace,
  CompareWorkspaceRepository,
} from '../compareWorkspaceModel.ts';

function forgetWithinScope(
  scope: CompareScopeWorkspace,
  resultKey: CompareResultKey,
): CompareScopeWorkspace {
  const activeForgotten = scope.active?.key === resultKey;
  const candidateForgotten = scope.candidate?.workspace.key === resultKey;
  const dismissalForgotten = scope.dismissedCandidateKey === resultKey;
  const cursorForgotten = scope.latestAutoScanPublication?.resultKey === resultKey;
  const executionForgotten = scope.execution?.status === 'fresh'
    && scope.execution.owner.identity.result_id === resultKey;
  if (!activeForgotten
    && !candidateForgotten
    && !dismissalForgotten
    && !cursorForgotten
    && !executionForgotten) return scope;

  // A candidate exists only as the newer alternative to an active result. When the active result
  // is the one being forgotten there is nothing left to choose between, so the candidate takes the
  // slot rather than being stranded behind an empty workspace.
  return {
    ...scope,
    active: activeForgotten ? scope.candidate?.workspace ?? null : scope.active,
    candidate: activeForgotten || candidateForgotten ? null : scope.candidate,
    dismissedCandidateKey: dismissalForgotten ? null : scope.dismissedCandidateKey,
    latestAutoScanPublication: cursorForgotten ? null : scope.latestAutoScanPublication,
    execution: executionForgotten ? null : scope.execution,
  };
}

export function forgetRetainedResult(
  repository: CompareWorkspaceRepository,
  resultKey: CompareResultKey,
): CompareWorkspaceRepository {
  let changed = false;
  const scopes = repository.scopes.map((scope) => {
    const next = forgetWithinScope(scope, resultKey);
    if (next !== scope) changed = true;
    return next;
  });
  return changed ? { scopes } : repository;
}
