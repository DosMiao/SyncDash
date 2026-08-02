// Repository transitions own cross-result ordering, publication, retention, and async request
// fencing. Per-result review transitions stay independent of these lifecycle rules.

import type { CompareScopeDto } from '../../core/types/generated/CompareScopeDto.ts';
import type { CompareScopeExecutionStatusDto } from '../../core/types/generated/CompareScopeExecutionStatusDto.ts';
import type { CompareWorkspaceLookupDto } from '../../core/types/generated/CompareWorkspaceLookupDto.ts';
import type { CompareWorkspaceSnapshotDto } from '../../core/types/generated/CompareWorkspaceSnapshotDto.ts';
import {
  compareResultKey,
  compareScopeFromIdentity,
  compareScopeKey,
  createCompareWorkspace,
  defaultCompareWorkspacePreferences,
  sameCompareScope,
} from './compareWorkspaceModel.ts';
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
} from './compareWorkspaceModel.ts';
import {
  deriveWorkspaceExecutionAccess,
  isConsistentCompareExecutionStatus,
  isConsistentComparePublication,
  isConsistentCompareWorkspaceSnapshot,
  reconcileExecutionStatus,
} from './compareWorkspaceExecution.ts';
import { reduceCompareWorkspaceReview } from './compareWorkspaceReview.ts';
import type { CompareWorkspaceReviewAction } from './compareWorkspaceReview.ts';

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
  | { type: 'job_display_name_rebound'; jobId: string; jobName: string }
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

export type CompareWorkspaceAction = CompareWorkspaceRepositoryAction | CompareWorkspaceReviewAction;

function createScopeWorkspace(scope: CompareScopeDto): CompareScopeWorkspace {
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

function promoteScope(
  repository: CompareWorkspaceRepository,
  scope: CompareScopeWorkspace,
): CompareWorkspaceRepository {
  return {
    scopes: [scope, ...repository.scopes.filter((candidate) => candidate.key !== scope.key)],
  };
}

function replaceScope(
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

function replaceExactWorkspace(
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

function replaceExecutableActiveWorkspace(
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

function beginExactWorkspaceLookup(
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

function completeExactWorkspaceLookup(
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

function rebindDisplayName(workspace: CompareWorkspace, jobName: string): CompareWorkspace {
  return workspace.display.jobName === jobName
    ? workspace
    : { ...workspace, display: { jobName } };
}

function refreshPublishedWorkspace(
  existing: CompareWorkspace,
  published: CompareWorkspace,
  retentionRequestId = existing.retention.requestId,
): CompareWorkspace {
  if (existing.key !== published.key) return published;
  return {
    ...existing,
    display: published.display,
    retention: { status: 'retained', requestId: retentionRequestId },
  };
}

function markWorkspaceMissing(workspace: CompareWorkspace, requestId: number): CompareWorkspace {
  return { ...workspace, retention: { status: 'missing', requestId } };
}

function expireJobExecution(
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

function finishPendingRestoration(scope: CompareScopeWorkspace): ScopeRestorationState {
  return scope.restoration.status === 'loading'
    ? { status: 'idle', requestId: scope.restoration.requestId }
    : scope.restoration;
}

function publishManualWorkspace(
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

function publishAutoScanWorkspace(
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

function completeScopeRestoration(
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

export function reduceCompareWorkspaces(
  repository: CompareWorkspaceRepository,
  action: CompareWorkspaceAction,
): CompareWorkspaceRepository {
  switch (action.type) {
    case 'scope_touched': {
      const key = compareScopeKey(action.scope);
      const scope = repository.scopes.find((candidate) => candidate.key === key);
      return scope ? promoteScope(repository, scope) : repository;
    }
    case 'execution_status_received': {
      const key = compareScopeKey(action.execution.scope);
      const existing = repository.scopes.find((scope) => scope.key === key);
      if (!existing) return repository;
      return replaceScope(repository, key, (scope) => {
        const execution = reconcileExecutionStatus(scope.execution, action.execution);
        return execution === scope.execution ? scope : { ...scope, execution };
      });
    }
    case 'scope_restore_started': {
      const key = compareScopeKey(action.scope);
      const current = repository.scopes.find((scope) => scope.key === key) ?? createScopeWorkspace(action.scope);
      if (action.requestId <= current.restoration.requestId) return repository;
      return promoteScope(repository, {
        ...current,
        restoration: { status: 'loading', requestId: action.requestId },
      });
    }
    case 'scope_restore_failed':
      return replaceScope(repository, action.scopeKey, (scope) => (
        scope.restoration.status === 'loading' && scope.restoration.requestId === action.requestId
          ? { ...scope, restoration: { status: 'failed', requestId: action.requestId, error: action.error } }
          : scope
      ));
    case 'scope_restore_completed':
      return completeScopeRestoration(
        repository,
        action.scopeKey,
        action.requestId,
        action.lookup,
        action.preferences ?? defaultCompareWorkspacePreferences,
      );
    case 'manual_compare_published':
      return publishManualWorkspace(
        repository,
        action.snapshot,
        action.preferences ?? defaultCompareWorkspacePreferences,
      );
    case 'autoscan_compare_published':
      return publishAutoScanWorkspace(
        repository,
        action.snapshot,
        action.generation,
        action.ticketId,
        action.preferences ?? defaultCompareWorkspacePreferences,
      );
    case 'candidate_adopted':
      return replaceScope(repository, action.scopeKey, (scope) => {
        if (scope.candidate?.workspace.key !== action.expectedResultKey) return scope;
        return {
          ...scope,
          active: scope.candidate.workspace,
          candidate: null,
          dismissedCandidateKey: null,
        };
      });
    case 'candidate_discarded':
      return replaceScope(repository, action.scopeKey, (scope) => {
        if (scope.candidate?.workspace.key !== action.expectedResultKey) return scope;
        return {
          ...scope,
          candidate: null,
          dismissedCandidateKey: action.expectedResultKey,
        };
      });
    case 'workspace_lookup_started':
      return beginExactWorkspaceLookup(repository, action.workspace, action.requestId);
    case 'workspace_lookup_completed':
      return completeExactWorkspaceLookup(
        repository,
        action.resultKey,
        action.requestId,
        action.lookup,
      );
    case 'workspace_lookup_failed':
      return replaceExactWorkspace(repository, action.resultKey, (workspace) => (
        workspace.retention.status === 'checking' && workspace.retention.requestId === action.requestId
          ? {
            ...workspace,
            retention: { status: 'check_failed', requestId: action.requestId, error: action.error },
          }
          : workspace
      ));
    case 'compare_activity_started': {
      const key = compareScopeKey(action.scope);
      const current = repository.scopes.find((scope) => scope.key === key) ?? createScopeWorkspace(action.scope);
      if (action.requestId <= current.activity.requestId) return repository;
      return promoteScope(repository, {
        ...current,
        activity: { status: 'comparing', requestId: action.requestId, origin: action.origin },
      });
    }
    case 'compare_activity_finished':
      return replaceScope(repository, action.scopeKey, (scope) => (
        scope.activity.requestId === action.requestId && scope.activity.status !== 'idle'
          ? { ...scope, activity: { status: 'idle', requestId: action.requestId } }
          : scope
      ));
    case 'compare_activity_failed':
      return replaceScope(repository, action.scopeKey, (scope) => (
        scope.activity.requestId === action.requestId
          && scope.activity.status === 'comparing'
          ? {
            ...scope,
            activity: {
              status: 'failed',
              requestId: action.requestId,
              origin: scope.activity.origin,
              error: action.error,
            },
          }
          : scope
      ));
    case 'row_inclusion_replaced':
    case 'row_reversal_replaced':
      return replaceExecutableActiveWorkspace(
        repository,
        action.resultKey,
        (workspace) => reduceCompareWorkspaceReview(workspace, action),
      );
    case 'result_view_changed':
    case 'selected_result_types_changed':
    case 'difference_search_draft_changed':
    case 'difference_search_applied':
    case 'folder_scope_changed':
    case 'advanced_filter_applied':
    case 'mask_resolution_started':
    case 'mask_resolution_succeeded':
    case 'mask_resolution_failed':
    case 'scope_folder_expansion_toggled':
    case 'scope_panel_collapsed_changed':
    case 'difference_sort_changed':
    case 'difference_grouping_changed':
    case 'path_mode_changed':
    case 'difference_folder_fold_toggled':
    case 'difference_folds_replaced':
    case 'difference_viewport_changed':
    case 'identical_search_draft_changed':
    case 'identical_search_applied':
    case 'identical_initial_load_started':
    case 'identical_load_more_started':
    case 'identical_page_loaded':
    case 'identical_page_failed':
    case 'identical_viewport_changed':
      return replaceExactWorkspace(
        repository,
        action.resultKey,
        (workspace) => reduceCompareWorkspaceReview(workspace, action),
      );
    case 'job_display_name_rebound': {
      let changed = false;
      const scopes = repository.scopes.map((scope) => {
        if (scope.scope.job_id !== action.jobId) return scope;
        const active = scope.active ? rebindDisplayName(scope.active, action.jobName) : null;
        const candidate = scope.candidate
          ? { ...scope.candidate, workspace: rebindDisplayName(scope.candidate.workspace, action.jobName) }
          : null;
        if (active !== scope.active || candidate?.workspace !== scope.candidate?.workspace) changed = true;
        return active === scope.active && candidate?.workspace === scope.candidate?.workspace
          ? scope
          : { ...scope, active, candidate };
      });
      return changed ? { scopes } : repository;
    }
    case 'job_execution_expired': {
      let changed = false;
      const scopes = repository.scopes.map((scope) => {
        const applies = scope.scope.job_id === action.jobId
          && (action.reason === 'job_deleted'
            || scope.scope.config_revision === action.configRevision);
        if (!applies) return scope;
        const execution = expireJobExecution(scope.execution, action.reason);
        if (execution === scope.execution) return scope;
        changed = true;
        return { ...scope, execution };
      });
      return changed ? { scopes } : repository;
    }
  }
}
