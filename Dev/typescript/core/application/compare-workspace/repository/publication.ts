// Publishing a completed Compare, for manual, AutoScan and restored runs.
// The three differ in who authorized them and therefore in what execution authority the resulting
// workspace carries, so they stay apart — an unattended publication must not inherit a manual
// one's.

import type { CompareWorkspaceLookupDto } from '#core/types/generated/CompareWorkspaceLookupDto.ts';
import type { CompareWorkspaceSnapshotDto } from '#core/types/generated/CompareWorkspaceSnapshotDto.ts';
import {
  compareScopeFromIdentity,
  compareScopeKey,
  createCompareWorkspace,
  sameCompareScope,
} from '../compareWorkspaceModel.ts';
import type {
  CompareScopeKey,
  CompareScopeWorkspace,
  CompareWorkspacePreferences,
  CompareWorkspaceRepository,
  ScopeRestorationState,
  StagedCompareCandidate,
} from '../compareWorkspaceModel.ts';
import {
  isConsistentComparePublication,
  reconcileExecutionStatus,
} from '../compareWorkspaceExecution.ts';
import { createScopeWorkspace, promoteScope, replaceScope } from './scopeIndex.ts';
import { scopeWorkspaceLookupProblem } from './lookup.ts';
import { finishPendingRestoration, markWorkspaceMissing, refreshPublishedWorkspace } from './lifecycle.ts';

export function publishManualWorkspace(
  repository: CompareWorkspaceRepository,
  snapshot: CompareWorkspaceSnapshotDto,
  preferences: CompareWorkspacePreferences,
): CompareWorkspaceRepository {
  if (!isConsistentComparePublication(snapshot)) return repository;
  const { plan, execution_status: execution } = snapshot;
  const scopeIdentity = compareScopeFromIdentity(plan.owner.identity);
  if (!sameCompareScope(scopeIdentity, execution.scope)) return repository;
  const key = compareScopeKey(scopeIdentity);
  const current = repository.scopes.find((scope) => scope.key === key) ?? createScopeWorkspace(scopeIdentity);
  const workspace = createCompareWorkspace(plan, preferences);
  const reconciledExecution = reconcileExecutionStatus(current.execution, execution);
  if (reconciledExecution !== execution) return repository;
  const publicationScope: CompareScopeWorkspace = {
    ...current,
    execution: reconciledExecution,
    restoration: finishPendingRestoration(current),
  };
  const existing = current.active?.key === workspace.key
    ? current.active
    : current.candidate?.workspace.key === workspace.key
      ? current.candidate.workspace
      : null;
  const published = existing ? refreshPublishedWorkspace(existing, workspace) : workspace;
  return promoteScope(repository, {
    ...publicationScope,
    active: published,
    candidate: null,
    dismissedCandidateKey: null,
  });
}

export function publishAutoScanWorkspace(
  repository: CompareWorkspaceRepository,
  snapshot: CompareWorkspaceSnapshotDto,
  generation: number,
  ticketId: number,
  preferences: CompareWorkspacePreferences,
): CompareWorkspaceRepository {
  if (!isConsistentComparePublication(snapshot)) return repository;
  const { plan, execution_status: execution } = snapshot;
  const scopeIdentity = compareScopeFromIdentity(plan.owner.identity);
  if (!sameCompareScope(scopeIdentity, execution.scope)) return repository;
  const key = compareScopeKey(scopeIdentity);
  const current = repository.scopes.find((scope) => scope.key === key) ?? createScopeWorkspace(scopeIdentity);
  const published = createCompareWorkspace(plan, preferences);
  const reconciledExecution = reconcileExecutionStatus(current.execution, execution);
  if (reconciledExecution !== execution) return repository;
  const cursor = current.latestAutoScanPublication;
  const publicationIsOlder = cursor
    && (generation < cursor.generation
      || (generation === cursor.generation && ticketId < cursor.ticketId));
  const publicationContradictsCursor = cursor
    && generation === cursor.generation
    && ticketId === cursor.ticketId
    && cursor.resultKey !== published.key;
  if (publicationIsOlder || publicationContradictsCursor) {
    return reconciledExecution === current.execution
      ? repository
      : promoteScope(repository, { ...current, execution: reconciledExecution });
  }
  const latestAutoScanPublication = { generation, ticketId, resultKey: published.key };
  if (current.dismissedCandidateKey === published.key) {
    return promoteScope(repository, {
      ...current,
      execution: reconciledExecution,
      latestAutoScanPublication,
    });
  }
  const publicationScope: CompareScopeWorkspace = {
    ...current,
    execution: reconciledExecution,
    latestAutoScanPublication,
    restoration: finishPendingRestoration(current),
  };
  if (current.active?.key === published.key) {
    return promoteScope(repository, {
      ...publicationScope,
      active: refreshPublishedWorkspace(current.active, published),
    });
  }
  if (!current.active) {
    return promoteScope(repository, {
      ...publicationScope,
      active: published,
      dismissedCandidateKey: null,
    });
  }
  const candidate: StagedCompareCandidate = current.candidate?.workspace.key === published.key
    ? { ...current.candidate, workspace: refreshPublishedWorkspace(current.candidate.workspace, published) }
    : {
      workspace: published,
      origin: { kind: 'auto_scan', generation, ticketId },
    };
  return promoteScope(repository, {
    ...publicationScope,
    candidate,
    dismissedCandidateKey: null,
  });
}

export function completeScopeRestoration(
  repository: CompareWorkspaceRepository,
  scopeKeyValue: CompareScopeKey,
  requestId: number,
  lookup: CompareWorkspaceLookupDto,
  preferences: CompareWorkspacePreferences,
): CompareWorkspaceRepository {
  const index = repository.scopes.findIndex((scope) => scope.key === scopeKeyValue);
  if (index < 0) return repository;
  const current = repository.scopes[index];
  if (current.restoration.status !== 'loading' || current.restoration.requestId !== requestId) return repository;
  const lookupProblem = scopeWorkspaceLookupProblem(current.scope, lookup);
  if (lookupProblem) {
    return replaceScope(repository, scopeKeyValue, (scope) => ({
      ...scope,
      restoration: { status: 'failed', requestId, error: lookupProblem },
    }));
  }
  if (lookup.status === 'missing') {
    const active = current.active ? markWorkspaceMissing(current.active, requestId) : null;
    const candidate = current.candidate
      ? { ...current.candidate, workspace: markWorkspaceMissing(current.candidate.workspace, requestId) }
      : null;
    return promoteScope(repository, {
      ...current,
      active,
      candidate,
      execution: reconcileExecutionStatus(current.execution, lookup.execution_status),
      restoration: { status: 'idle', requestId },
    });
  }
  const { plan, execution_status: execution } = lookup.workspace;
  const published = createCompareWorkspace(plan, preferences);
  const reconciledExecution = reconcileExecutionStatus(current.execution, execution);
  const restoration: ScopeRestorationState = { status: 'idle', requestId };
  if (current.dismissedCandidateKey === published.key) {
    return promoteScope(repository, { ...current, execution: reconciledExecution, restoration });
  }
  if (current.active?.key === published.key) {
    return promoteScope(repository, {
      ...current,
      execution: reconciledExecution,
      active: refreshPublishedWorkspace(current.active, published),
      restoration,
    });
  }
  if (current.candidate?.workspace.key === published.key) {
    return promoteScope(repository, {
      ...current,
      execution: reconciledExecution,
      candidate: {
        ...current.candidate,
        workspace: refreshPublishedWorkspace(current.candidate.workspace, published),
      },
      restoration,
    });
  }
  if (!current.active) {
    return promoteScope(repository, {
      ...current,
      execution: reconciledExecution,
      active: published,
      restoration,
    });
  }
  return replaceScope(repository, scopeKeyValue, (scope) => ({
    ...scope,
    restoration: {
      status: 'failed',
      requestId,
      error: 'A newer local result completed before this restore response',
    },
  }));
}
