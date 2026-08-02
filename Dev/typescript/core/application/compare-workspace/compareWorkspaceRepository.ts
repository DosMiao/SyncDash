// Repository transitions own cross-result ordering, publication, retention, and async request
// fencing. Per-result review transitions stay independent of these lifecycle rules.

import type { CompareScopeDto } from '#core/types/generated/CompareScopeDto.ts';
import type { CompareScopeExecutionStatusDto } from '#core/types/generated/CompareScopeExecutionStatusDto.ts';
import type { CompareWorkspaceLookupDto } from '#core/types/generated/CompareWorkspaceLookupDto.ts';
import type { CompareWorkspaceSnapshotDto } from '#core/types/generated/CompareWorkspaceSnapshotDto.ts';
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
import { createScopeWorkspace, promoteScope, replaceExactWorkspace, replaceExecutableActiveWorkspace, replaceScope } from './repository/scopeIndex.ts';
import { beginExactWorkspaceLookup, completeExactWorkspaceLookup, exactWorkspaceLookupProblem, scopeWorkspaceLookupProblem } from './repository/lookup.ts';
import { expireJobExecution, finishPendingRestoration, markWorkspaceMissing, refreshPublishedWorkspace } from './repository/lifecycle.ts';
import { completeScopeRestoration, publishAutoScanWorkspace, publishManualWorkspace } from './repository/publication.ts';

export type CompareWorkspaceAction = CompareWorkspaceRepositoryAction | CompareWorkspaceReviewAction;

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
