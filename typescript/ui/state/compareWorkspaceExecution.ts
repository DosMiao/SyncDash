// Execution authority is reconciled separately from retained review evidence so an out-of-order
// status response can never re-authorize an older exact result.

import type { CompareScopeExecutionStatusDto } from '../../core/types/generated/CompareScopeExecutionStatusDto.ts';
import type { CompareVerificationAttemptDto } from '../../core/types/generated/CompareVerificationAttemptDto.ts';
import type { CompareWorkspaceSnapshotDto } from '../../core/types/generated/CompareWorkspaceSnapshotDto.ts';
import type { CompareIdentity } from '../../core/types/generated/CompareIdentity.ts';
import {
  compareScopeFromIdentity,
  sameCompareIdentity,
  sameCompareScope,
} from './compareWorkspaceModel.ts';
import type { CompareWorkspace } from './compareWorkspaceModel.ts';

export type WorkspaceViewOnlyReason =
  | 'retention_checking'
  | 'retention_check_failed'
  | 'retention_missing'
  | 'execution_unavailable'
  | 'awaiting_compare'
  | 'compare_in_progress'
  | 'compare_failed'
  | 'compare_cancelled'
  | 'superseded'
  | 'execution_expired';

export type WorkspaceExecutionAccess =
  | { status: 'executable'; verificationEpoch: number }
  | { status: 'view_only'; reason: WorkspaceViewOnlyReason; replacement: CompareIdentity | null };

export function deriveWorkspaceExecutionAccess(
  workspace: CompareWorkspace,
  execution: CompareScopeExecutionStatusDto | null,
): WorkspaceExecutionAccess {
  switch (workspace.retention.status) {
    case 'checking':
      return { status: 'view_only', reason: 'retention_checking', replacement: null };
    case 'check_failed':
      return { status: 'view_only', reason: 'retention_check_failed', replacement: null };
    case 'missing':
      return { status: 'view_only', reason: 'retention_missing', replacement: null };
    case 'retained':
      break;
  }
  if (!execution || !sameCompareScope(compareScopeFromIdentity(workspace.identity), execution.scope)) {
    return { status: 'view_only', reason: 'execution_unavailable', replacement: null };
  }
  switch (execution.status) {
    case 'unavailable':
      return { status: 'view_only', reason: 'execution_unavailable', replacement: null };
    case 'awaiting_compare':
      return { status: 'view_only', reason: 'awaiting_compare', replacement: null };
    case 'comparing':
      return { status: 'view_only', reason: 'compare_in_progress', replacement: null };
    case 'failed':
      return { status: 'view_only', reason: 'compare_failed', replacement: null };
    case 'cancelled':
      return { status: 'view_only', reason: 'compare_cancelled', replacement: null };
    case 'expired':
      return { status: 'view_only', reason: 'execution_expired', replacement: null };
    case 'fresh':
      return sameCompareIdentity(execution.owner.identity, workspace.identity)
        ? { status: 'executable', verificationEpoch: execution.attempt.verification_epoch }
        : { status: 'view_only', reason: 'superseded', replacement: execution.owner.identity };
  }
}

function sameVerificationAttempt(
  left: CompareVerificationAttemptDto,
  right: CompareVerificationAttemptDto,
): boolean {
  return left.verification_epoch === right.verification_epoch
    && left.compare_run_id === right.compare_run_id;
}

export function isConsistentCompareExecutionStatus(status: CompareScopeExecutionStatusDto): boolean {
  if (status.status === 'unavailable') return true;
  if (status.attempt.verification_epoch < 1) return false;
  switch (status.status) {
    case 'awaiting_compare':
      return status.attempt.compare_run_id === null;
    case 'comparing':
      return status.attempt.compare_run_id !== null;
    case 'fresh':
      return status.attempt.compare_run_id === status.owner.identity.compare_run_id
        && sameCompareScope(status.scope, compareScopeFromIdentity(status.owner.identity));
    case 'failed':
    case 'cancelled':
    case 'expired':
      return true;
  }
}

export function isConsistentCompareWorkspaceSnapshot(snapshot: CompareWorkspaceSnapshotDto): boolean {
  return isConsistentCompareExecutionStatus(snapshot.execution_status)
    && sameCompareScope(
      compareScopeFromIdentity(snapshot.plan.owner.identity),
      snapshot.execution_status.scope,
    );
}

export function isConsistentComparePublication(snapshot: CompareWorkspaceSnapshotDto): boolean {
  return isConsistentCompareWorkspaceSnapshot(snapshot)
    && snapshot.execution_status.status === 'fresh'
    && sameCompareIdentity(snapshot.plan.owner.identity, snapshot.execution_status.owner.identity);
}

function canAdvanceAtSameVerificationEpoch(
  current: Exclude<CompareScopeExecutionStatusDto, { status: 'unavailable' }>,
  incoming: Exclude<CompareScopeExecutionStatusDto, { status: 'unavailable' }>,
): boolean {
  if (current.status === incoming.status) return true;
  if (current.status === 'expired') return false;
  if (incoming.status === 'expired') return true;
  if (current.status === 'awaiting_compare') return true;
  if (current.status === 'comparing') {
    return incoming.status === 'fresh'
      || incoming.status === 'failed'
      || incoming.status === 'cancelled';
  }
  return false;
}

export function reconcileExecutionStatus(
  current: CompareScopeExecutionStatusDto | null,
  incoming: CompareScopeExecutionStatusDto,
): CompareScopeExecutionStatusDto | null {
  if (!isConsistentCompareExecutionStatus(incoming)) return current;
  if (!current) return incoming;
  if (!sameCompareScope(current.scope, incoming.scope)) return current;
  if (current.status === 'unavailable') return incoming;
  if (incoming.status === 'unavailable') return current;
  const currentEpoch = current.attempt.verification_epoch;
  const incomingEpoch = incoming.attempt.verification_epoch;
  if (incomingEpoch < currentEpoch) return current;
  if (incomingEpoch > currentEpoch) return incoming;
  if (current.status !== 'awaiting_compare'
    && !sameVerificationAttempt(current.attempt, incoming.attempt)) return current;
  return canAdvanceAtSameVerificationEpoch(current, incoming) ? incoming : current;
}
