// Republication, disappearance and expiry of a workspace already in the index.
// Expiry marks a result unexecutable without deleting it: the plan stays viewable, which is the
// distinction the immutable-evidence rule rests on.

import type { CompareScopeExecutionStatusDto } from '#core/types/generated/CompareScopeExecutionStatusDto.ts';
import type {
  CompareScopeWorkspace,
  CompareWorkspace,
  ScopeRestorationState,
} from '../compareWorkspaceModel.ts';

export function refreshPublishedWorkspace(
  existing: CompareWorkspace,
  published: CompareWorkspace,
  retentionRequestId = existing.retention.requestId,
): CompareWorkspace {
  if (existing.key !== published.key) return published;
  return {
    ...existing,
    retention: { status: 'retained', requestId: retentionRequestId },
  };
}

export function markWorkspaceMissing(workspace: CompareWorkspace, requestId: number): CompareWorkspace {
  return { ...workspace, retention: { status: 'missing', requestId } };
}

export function expireJobExecution(
  execution: CompareScopeExecutionStatusDto | null,
  reason: 'job_changed' | 'job_deleted',
): CompareScopeExecutionStatusDto | null {
  if (!execution || execution.status === 'unavailable' || execution.status === 'expired') return execution;
  return {
    status: 'expired',
    scope: execution.scope,
    attempt: execution.attempt,
    reason,
  };
}

export function finishPendingRestoration(scope: CompareScopeWorkspace): ScopeRestorationState {
  return scope.restoration.status === 'loading'
    ? { status: 'idle', requestId: scope.restoration.requestId }
    : scope.restoration;
}
