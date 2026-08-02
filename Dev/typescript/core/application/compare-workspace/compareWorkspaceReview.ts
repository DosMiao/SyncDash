// These transitions mutate one exact retained workspace. Repository routing separately decides
// which review actions require current execution authority and which remain available view-only.

import type { ResultType, Sort } from '#core/domain/compare/plan.ts';
import type { AdvancedScopeFilter } from '#core/domain/compare/runScope.ts';
import type { IdenticalRow } from '#core/types/generated/IdenticalRow.ts';
import {
  advancedFilterMasksEqual,
  advancedFiltersEqual,
  validatedAdvancedFilter,
} from './compareWorkspaceFilters.ts';
import type {
  CompareResultKey,
  CompareResultView,
  CompareWorkspace,
  DifferenceViewport,
  DifferenceWorkspace,
  IdenticalViewport,
  MaskResolutionState,
} from './compareWorkspaceModel.ts';

export type CompareWorkspaceReviewAction =
  | { type: 'row_inclusion_replaced'; resultKey: CompareResultKey; rowIncluded: boolean[] }
  | { type: 'row_reversal_replaced'; resultKey: CompareResultKey; rowReversed: boolean[] }
  | { type: 'result_view_changed'; resultKey: CompareResultKey; view: CompareResultView }
  | { type: 'selected_result_types_changed'; resultKey: CompareResultKey; resultTypes: ReadonlySet<ResultType> }
  | { type: 'difference_search_draft_changed'; resultKey: CompareResultKey; requestId: number; draft: string }
  | { type: 'difference_search_applied'; resultKey: CompareResultKey; requestId: number; query: string }
  | { type: 'folder_scope_changed'; resultKey: CompareResultKey; folderScope: string | null }
  | { type: 'advanced_filter_applied'; resultKey: CompareResultKey; appliedFilter: AdvancedScopeFilter }
  | { type: 'mask_resolution_started'; resultKey: CompareResultKey; inputRevision: number; requestId: number }
  | {
    type: 'mask_resolution_succeeded';
    resultKey: CompareResultKey;
    inputRevision: number;
    requestId: number;
    excludedByRow: boolean[];
  }
  | {
    type: 'mask_resolution_failed';
    resultKey: CompareResultKey;
    inputRevision: number;
    requestId: number;
    error: string;
  }
  | { type: 'scope_folder_expansion_toggled'; resultKey: CompareResultKey; folderPath: string }
  | { type: 'scope_panel_collapsed_changed'; resultKey: CompareResultKey; collapsed: boolean }
  | { type: 'difference_sort_changed'; resultKey: CompareResultKey; sort: Sort | null }
  | { type: 'difference_grouping_changed'; resultKey: CompareResultKey; grouped: boolean }
  | { type: 'path_mode_changed'; resultKey: CompareResultKey; pathMode: 'relative' | 'full' }
  | { type: 'difference_folder_fold_toggled'; resultKey: CompareResultKey; folderPath: string }
  | { type: 'difference_folds_replaced'; resultKey: CompareResultKey; collapsedFolders: ReadonlySet<string> }
  | { type: 'difference_viewport_changed'; resultKey: CompareResultKey; viewport: DifferenceViewport }
  | { type: 'identical_search_draft_changed'; resultKey: CompareResultKey; requestId: number; draft: string }
  | { type: 'identical_search_applied'; resultKey: CompareResultKey; requestId: number; query: string }
  | { type: 'identical_initial_load_started'; resultKey: CompareResultKey; requestId: number; query: string }
  | {
    type: 'identical_load_more_started';
    resultKey: CompareResultKey;
    requestId: number;
    query: string;
    offset: number;
  }
  | {
    type: 'identical_page_loaded';
    resultKey: CompareResultKey;
    requestId: number;
    query: string;
    offset: number;
    rows: IdenticalRow[];
    total: number;
  }
  | { type: 'identical_page_failed'; resultKey: CompareResultKey; requestId: number; query: string; error: string }
  | { type: 'identical_viewport_changed'; resultKey: CompareResultKey; viewport: IdenticalViewport };

function sameBooleanArray(left: readonly boolean[], right: readonly boolean[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function sameResultTypes(left: ReadonlySet<ResultType>, right: ReadonlySet<ResultType>): boolean {
  return left.size === right.size && [...left].every((value) => right.has(value));
}

function maskResolutionForInput(maskCount: number, inputRevision: number): MaskResolutionState {
  return maskCount === 0
    ? { status: 'not_required', inputRevision }
    : { status: 'unresolved', inputRevision };
}

function withReviewMutation(
  workspace: CompareWorkspace,
  differences: DifferenceWorkspace,
): CompareWorkspace {
  return { ...workspace, differences, reviewRevision: workspace.reviewRevision + 1 };
}

export function reduceCompareWorkspaceReview(
  workspace: CompareWorkspace,
  action: CompareWorkspaceReviewAction,
): CompareWorkspace {
  switch (action.type) {
    case 'row_inclusion_replaced': {
      if (action.rowIncluded.length !== workspace.plan.ops.length
        || sameBooleanArray(action.rowIncluded, workspace.differences.rowIncluded)) return workspace;
      return withReviewMutation(workspace, {
        ...workspace.differences,
        rowIncluded: [...action.rowIncluded],
      });
    }
    case 'row_reversal_replaced': {
      if (action.rowReversed.length !== workspace.plan.ops.length
        || sameBooleanArray(action.rowReversed, workspace.differences.rowReversed)) return workspace;
      const inputRevision = workspace.differences.maskInputRevision + 1;
      return withReviewMutation(workspace, {
        ...workspace.differences,
        rowReversed: [...action.rowReversed],
        maskInputRevision: inputRevision,
        maskResolution: maskResolutionForInput(
          workspace.differences.appliedAdvancedFilter.masks.length,
          inputRevision,
        ),
        viewport: { ...workspace.differences.viewport, logicalTop: 0 },
      });
    }
    case 'result_view_changed':
      return workspace.selectedView === action.view
        ? workspace
        : { ...workspace, selectedView: action.view };
    case 'selected_result_types_changed': {
      if (sameResultTypes(workspace.differences.selectedResultTypes, action.resultTypes)) return workspace;
      return withReviewMutation(workspace, {
        ...workspace.differences,
        selectedResultTypes: new Set(action.resultTypes),
        viewport: { ...workspace.differences.viewport, logicalTop: 0 },
      });
    }
    case 'difference_search_draft_changed': {
      if (action.requestId <= workspace.differences.searchRequestId) return workspace;
      if (action.draft === workspace.differences.searchDraft) return workspace;
      return withReviewMutation(workspace, {
        ...workspace.differences,
        searchDraft: action.draft,
        searchRequestId: action.requestId,
      });
    }
    case 'difference_search_applied':
      return workspace.differences.searchRequestId !== action.requestId
        || workspace.differences.appliedSearch === action.query
        ? workspace
        : {
          ...workspace,
          differences: {
            ...workspace.differences,
            appliedSearch: action.query,
            viewport: { ...workspace.differences.viewport, logicalTop: 0 },
          },
        };
    case 'folder_scope_changed':
      return workspace.differences.folderScope === action.folderScope
        ? workspace
        : withReviewMutation(workspace, {
          ...workspace.differences,
          folderScope: action.folderScope,
          viewport: { ...workspace.differences.viewport, logicalTop: 0 },
        });
    case 'advanced_filter_applied': {
      const appliedFilter = validatedAdvancedFilter(action.appliedFilter);
      if (advancedFiltersEqual(workspace.differences.appliedAdvancedFilter, appliedFilter)) return workspace;
      const masksChanged = !advancedFilterMasksEqual(
        workspace.differences.appliedAdvancedFilter,
        appliedFilter,
      );
      const inputRevision = masksChanged
        ? workspace.differences.maskInputRevision + 1
        : workspace.differences.maskInputRevision;
      return withReviewMutation(workspace, {
        ...workspace.differences,
        appliedAdvancedFilter: appliedFilter,
        maskInputRevision: inputRevision,
        maskResolution: masksChanged
          ? maskResolutionForInput(appliedFilter.masks.length, inputRevision)
          : workspace.differences.maskResolution,
        viewport: { ...workspace.differences.viewport, logicalTop: 0 },
      });
    }
    case 'mask_resolution_started':
      return workspace.differences.maskInputRevision !== action.inputRevision
        || workspace.differences.appliedAdvancedFilter.masks.length === 0
        ? workspace
        : {
          ...workspace,
          differences: {
            ...workspace.differences,
            maskResolution: {
              status: 'pending',
              inputRevision: action.inputRevision,
              requestId: action.requestId,
            },
          },
        };
    case 'mask_resolution_succeeded': {
      const resolution = workspace.differences.maskResolution;
      if (resolution.status !== 'pending'
        || resolution.inputRevision !== action.inputRevision
        || resolution.requestId !== action.requestId
        || action.excludedByRow.length !== workspace.plan.ops.length) return workspace;
      return {
        ...workspace,
        differences: {
          ...workspace.differences,
          maskResolution: {
            status: 'ready',
            inputRevision: action.inputRevision,
            requestId: action.requestId,
            excludedByRow: [...action.excludedByRow],
          },
          viewport: { ...workspace.differences.viewport, logicalTop: 0 },
        },
      };
    }
    case 'mask_resolution_failed': {
      const resolution = workspace.differences.maskResolution;
      if (resolution.status !== 'pending'
        || resolution.inputRevision !== action.inputRevision
        || resolution.requestId !== action.requestId) return workspace;
      return {
        ...workspace,
        differences: {
          ...workspace.differences,
          maskResolution: {
            status: 'failed',
            inputRevision: action.inputRevision,
            requestId: action.requestId,
            error: action.error,
          },
          viewport: { ...workspace.differences.viewport, logicalTop: 0 },
        },
      };
    }
    case 'scope_folder_expansion_toggled': {
      const expandedScopeFolders = new Set(workspace.differences.expandedScopeFolders);
      if (expandedScopeFolders.has(action.folderPath)) expandedScopeFolders.delete(action.folderPath);
      else expandedScopeFolders.add(action.folderPath);
      return {
        ...workspace,
        differences: { ...workspace.differences, expandedScopeFolders },
      };
    }
    case 'scope_panel_collapsed_changed':
      return workspace.differences.scopePanelCollapsed === action.collapsed
        ? workspace
        : {
          ...workspace,
          differences: { ...workspace.differences, scopePanelCollapsed: action.collapsed },
        };
    case 'difference_sort_changed':
      return workspace.differences.sort?.key === action.sort?.key
        && workspace.differences.sort?.dir === action.sort?.dir
        ? workspace
        : {
          ...workspace,
          differences: {
            ...workspace.differences,
            sort: action.sort,
            viewport: { ...workspace.differences.viewport, logicalTop: 0 },
          },
        };
    case 'difference_grouping_changed':
      return workspace.differences.grouped === action.grouped
        ? workspace
        : {
          ...workspace,
          differences: {
            ...workspace.differences,
            grouped: action.grouped,
            collapsedFolders: new Set(),
            viewport: { ...workspace.differences.viewport, logicalTop: 0 },
          },
        };
    case 'path_mode_changed':
      return workspace.differences.pathMode === action.pathMode
        ? workspace
        : { ...workspace, differences: { ...workspace.differences, pathMode: action.pathMode } };
    case 'difference_folder_fold_toggled': {
      const collapsedFolders = new Set(workspace.differences.collapsedFolders);
      if (collapsedFolders.has(action.folderPath)) collapsedFolders.delete(action.folderPath);
      else collapsedFolders.add(action.folderPath);
      return { ...workspace, differences: { ...workspace.differences, collapsedFolders } };
    }
    case 'difference_folds_replaced':
      return {
        ...workspace,
        differences: {
          ...workspace.differences,
          collapsedFolders: new Set(action.collapsedFolders),
        },
      };
    case 'difference_viewport_changed':
      return workspace.differences.viewport.logicalTop === action.viewport.logicalTop
        && workspace.differences.viewport.scrollLeft === action.viewport.scrollLeft
        ? workspace
        : {
          ...workspace,
          differences: { ...workspace.differences, viewport: { ...action.viewport } },
        };
    case 'identical_search_draft_changed':
      return action.requestId <= workspace.identical.searchRequestId
        || action.draft === workspace.identical.searchDraft
        ? workspace
        : {
          ...workspace,
          identical: {
            ...workspace.identical,
            searchDraft: action.draft,
            searchRequestId: action.requestId,
          },
        };
    case 'identical_search_applied':
      return workspace.identical.searchRequestId !== action.requestId
        || workspace.identical.appliedSearch === action.query
        ? workspace
        : {
          ...workspace,
          identical: {
            ...workspace.identical,
            appliedSearch: action.query,
            pages: { status: 'idle' },
            viewport: { ...workspace.identical.viewport, scrollTop: 0 },
          },
        };
    case 'identical_initial_load_started':
      return workspace.identical.appliedSearch !== action.query
        ? workspace
        : {
          ...workspace,
          identical: {
            ...workspace.identical,
            pages: { status: 'loading_initial', requestId: action.requestId, query: action.query },
          },
        };
    case 'identical_load_more_started': {
      const pages = workspace.identical.pages;
      if (workspace.identical.appliedSearch !== action.query
        || (pages.status !== 'ready' && pages.status !== 'load_more_failed')
        || pages.query !== action.query
        || pages.rows.length !== action.offset
        || pages.rows.length >= pages.total) return workspace;
      return {
        ...workspace,
        identical: {
          ...workspace.identical,
          pages: {
            status: 'loading_more',
            requestId: action.requestId,
            query: action.query,
            offset: action.offset,
            rows: pages.rows,
            total: pages.total,
          },
        },
      };
    }
    case 'identical_page_loaded': {
      const pages = workspace.identical.pages;
      if (pages.status === 'loading_initial') {
        if (pages.requestId !== action.requestId || pages.query !== action.query || action.offset !== 0) return workspace;
        return {
          ...workspace,
          identical: {
            ...workspace.identical,
            pages: { status: 'ready', query: action.query, rows: [...action.rows], total: action.total },
          },
        };
      }
      if (pages.status !== 'loading_more'
        || pages.requestId !== action.requestId
        || pages.query !== action.query
        || pages.offset !== action.offset) return workspace;
      return {
        ...workspace,
        identical: {
          ...workspace.identical,
          pages: {
            status: 'ready',
            query: action.query,
            rows: [...pages.rows, ...action.rows],
            total: action.total,
          },
        },
      };
    }
    case 'identical_page_failed': {
      const pages = workspace.identical.pages;
      if (pages.status === 'loading_initial') {
        if (pages.requestId !== action.requestId || pages.query !== action.query) return workspace;
        return {
          ...workspace,
          identical: {
            ...workspace.identical,
            pages: { status: 'initial_failed', query: action.query, error: action.error },
          },
        };
      }
      if (pages.status !== 'loading_more'
        || pages.requestId !== action.requestId
        || pages.query !== action.query) return workspace;
      return {
        ...workspace,
        identical: {
          ...workspace.identical,
          pages: {
            status: 'load_more_failed',
            query: action.query,
            rows: pages.rows,
            total: pages.total,
            error: action.error,
          },
        },
      };
    }
    case 'identical_viewport_changed':
      return workspace.identical.viewport.scrollTop === action.viewport.scrollTop
        && workspace.identical.viewport.scrollLeft === action.viewport.scrollLeft
        ? workspace
        : { ...workspace, identical: { ...workspace.identical, viewport: { ...action.viewport } } };
  }
}
