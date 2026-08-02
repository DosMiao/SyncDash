// Republication, disappearance and expiry of a workspace already in the index.
// Expiry marks a result unexecutable without deleting it: the plan stays viewable, which is the
// distinction the immutable-evidence rule rests on.

// Repository transitions own cross-result ordering, publication, retention, and async request
// fencing. Per-result review transitions stay independent of these lifecycle rules.
import type { CompareScopeDto } from '#core/types/generated/CompareScopeDto.ts';
import type { CompareWorkspaceLookupDto } from '#core/types/generated/CompareWorkspaceLookupDto.ts';
import type { CompareWorkspaceSnapshotDto } from '#core/types/generated/CompareWorkspaceSnapshotDto.ts';
import type { CompareScopeExecutionStatusDto } from '#core/types/generated/CompareScopeExecutionStatusDto.ts';
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
