import { useCallback, useId, useMemo, useRef } from 'react';
import type { Dispatch } from 'react';
import { deriveApplyAvailability } from '#core/application/apply/applyAvailability.ts';
import { deriveWorkspaceExecutionAccess } from '#core/application/compare-workspace/compareWorkspaceExecution.ts';
import {
  activeWorkspace,
  compareScopeForJob,
  scopeWorkspace,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type {
  CompareResultKey,
  CompareWorkspacePreferences,
  CompareWorkspaceRepository,
  DifferenceViewport,
  IdenticalViewport,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import { applyReviewKey, compareReviewKey } from '#core/application/operations/operationReview.ts';
import { buildLayout, flattenLayout, layoutFolderPaths } from '#core/domain/compare/grouping.ts';
import { buildReviewedRowDecisions } from '#core/domain/compare/plan.ts';
import {
  computeExecutableIndices,
  computeInScopeIndices,
  EMPTY_ADVANCED_SCOPE_FILTER,
} from '#core/domain/compare/runScope.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import { summarizeSelectedRun } from '#ui/features/compare-results/model/selectedRunStats.ts';
import { useCompareWorkspaceController } from '#ui/features/compare-workspace/hooks/useCompareWorkspaceController.ts';
import { useOwnedResultViewport } from '#ui/features/compare-workspace/hooks/useOwnedResultViewport.ts';
import {
  EMPTY_FLAGS,
  EMPTY_LAYOUT,
  EMPTY_PATH_SET,
  EMPTY_RESULT_TYPES,
} from '../model/workspacePageModel.ts';

interface WorkspaceResultViewModelOptions {
  repository: CompareWorkspaceRepository;
  selectedJob: JobDto | null;
  selectedTargetIndex: number;
  preferences: CompareWorkspacePreferences;
  resultViewportElement: HTMLDivElement | null;
  dispatch: Dispatch<CompareWorkspaceAction>;
  setStatus: (message: string, appearance?: 'ok' | 'err' | '') => void;
}

export function useWorkspaceResultViewModel({
  repository,
  selectedJob,
  selectedTargetIndex,
  preferences,
  resultViewportElement,
  dispatch,
  setStatus,
}: WorkspaceResultViewModelOptions) {
  const selectedScopeWorkspace = selectedJob
    ? scopeWorkspace(repository, compareScopeForJob(selectedJob, selectedTargetIndex))
    : null;
  const selectedCompareWorkspace = activeWorkspace(repository, selectedJob, selectedTargetIndex);
  const selectedCompareWorkspaceKeyRef = useRef<CompareResultKey | null>(selectedCompareWorkspace?.key ?? null);
  selectedCompareWorkspaceKeyRef.current = selectedCompareWorkspace?.key ?? null;

  const plan = selectedCompareWorkspace?.plan ?? null;
  const differenceWorkspace = selectedCompareWorkspace?.differences ?? null;
  const includedRows = differenceWorkspace?.rowIncluded ?? EMPTY_FLAGS;
  const reversedRows = differenceWorkspace?.rowReversed ?? EMPTY_FLAGS;
  const selectedResultTypes = differenceWorkspace?.selectedResultTypes ?? EMPTY_RESULT_TYPES;
  const searchDraft = differenceWorkspace?.searchDraft ?? '';
  const searchQuery = differenceWorkspace?.appliedSearch ?? '';
  const folderScope = differenceWorkspace?.folderScope ?? null;
  const appliedAdvancedFilter = differenceWorkspace?.appliedAdvancedFilter ?? EMPTY_ADVANCED_SCOPE_FILTER;
  const expandedFolders = differenceWorkspace?.expandedScopeFolders ?? EMPTY_PATH_SET;
  const panelCollapsed = differenceWorkspace?.scopePanelCollapsed ?? preferences.scopePanelCollapsed;
  const sort = differenceWorkspace?.sort ?? null;
  const grouped = differenceWorkspace?.grouped ?? preferences.grouped;
  const pathMode = differenceWorkspace?.pathMode ?? preferences.pathMode;
  const collapsedFolderPaths = differenceWorkspace?.collapsedFolders ?? EMPTY_PATH_SET;
  const resultView = selectedCompareWorkspace?.selectedView ?? 'differences';
  const workspaceExecutionAccess = selectedCompareWorkspace
    ? deriveWorkspaceExecutionAccess(selectedCompareWorkspace, selectedScopeWorkspace?.execution ?? null)
    : null;
  const reviewEditable = workspaceExecutionAccess?.status === 'executable'
    && selectedScopeWorkspace?.activity.status === 'idle';

  const reportRunScopeError = useCallback((message: string) => setStatus(message, 'err'), [setStatus]);
  const workspaceController = useCompareWorkspaceController(
    selectedCompareWorkspace,
    dispatch,
    reportRunScopeError,
  );
  const recordIdenticalViewport = useCallback((resultKey: CompareResultKey, viewport: IdenticalViewport) => {
    dispatch({ type: 'identical_viewport_changed', resultKey, viewport });
  }, [dispatch]);
  const recordDifferenceViewport = useCallback((resultKey: CompareResultKey, viewport: DifferenceViewport) => {
    dispatch({ type: 'difference_viewport_changed', resultKey, viewport });
  }, [dispatch]);
  useOwnedResultViewport<CompareResultKey>(
    selectedCompareWorkspace?.selectedView === 'identical' ? resultViewportElement : null,
    selectedCompareWorkspace?.selectedView === 'identical' ? selectedCompareWorkspace.key : null,
    selectedCompareWorkspace?.identical.viewport ?? { scrollTop: 0, scrollLeft: 0 },
    recordIdenticalViewport,
  );

  const searchPending = differenceWorkspace !== null && searchDraft.trim() !== searchQuery;
  const maskStatus = differenceWorkspace?.maskResolution.status ?? 'not_required';
  const excludedByMask = useMemo(() => {
    if (!plan || appliedAdvancedFilter.masks.length === 0) return [];
    if (differenceWorkspace?.maskResolution.status === 'ready') {
      return differenceWorkspace.maskResolution.excludedByRow;
    }
    return plan.ops.map(() => true);
  }, [appliedAdvancedFilter.masks.length, differenceWorkspace?.maskResolution, plan]);
  const scopeCalculationPending = searchPending || maskStatus === 'unresolved' || maskStatus === 'pending';
  const scopeCalculationFailed = maskStatus === 'failed';

  const resultPanelId = useId();
  const differencesTabId = `${resultPanelId}-differences-tab`;
  const identicalTabId = `${resultPanelId}-identical-tab`;

  const inScopeIndices = useMemo(() => (
    plan ? computeInScopeIndices({
      plan,
      reversedRows,
      selectedResultTypes,
      searchQuery,
      folderScope,
      advancedFilter: appliedAdvancedFilter,
      excludedByMask,
    }) : []
  ), [plan, reversedRows, selectedResultTypes, searchQuery, folderScope, appliedAdvancedFilter, excludedByMask]);
  const executableIndices = useMemo(() => (
    plan ? computeExecutableIndices(plan, reversedRows, inScopeIndices, includedRows) : []
  ), [plan, reversedRows, inScopeIndices, includedRows]);
  const reviewedRowDecisions = useMemo(
    () => (plan ? buildReviewedRowDecisions(executableIndices, reversedRows) : []),
    [plan, executableIndices, reversedRows],
  );
  const reviewKey = useMemo(() => (
    plan && selectedJob && workspaceExecutionAccess?.status === 'executable'
      ? applyReviewKey(
        plan.owner.identity,
        selectedJob.job_id,
        selectedJob.config_revision,
        selectedTargetIndex,
        workspaceExecutionAccess.verificationEpoch,
        reviewedRowDecisions,
      )
      : null
  ), [plan, selectedJob, selectedTargetIndex, workspaceExecutionAccess, reviewedRowDecisions]);
  const currentReviewKeyRef = useRef<string | null>(reviewKey);
  currentReviewKeyRef.current = reviewKey;
  const currentCompareReviewKey = useMemo(() => (
    selectedJob
      ? compareReviewKey(selectedJob.job_id, selectedJob.config_revision, selectedTargetIndex)
      : null
  ), [selectedJob, selectedTargetIndex]);
  const currentCompareReviewKeyRef = useRef<string | null>(currentCompareReviewKey);
  currentCompareReviewKeyRef.current = currentCompareReviewKey;

  const layout = useMemo(() => (
    plan ? buildLayout({ plan, reversedRows, inScopeIndices, grouped, sort }) : EMPTY_LAYOUT
  ), [plan, reversedRows, inScopeIndices, grouped, sort]);
  const rowPlan = useMemo(() => flattenLayout(layout, collapsedFolderPaths), [layout, collapsedFolderPaths]);
  const folderPathsInLayout = useMemo(() => layoutFolderPaths(layout), [layout]);
  const anyCollapsed = useMemo(
    () => folderPathsInLayout.some((folderPath) => collapsedFolderPaths.has(folderPath)),
    [folderPathsInLayout, collapsedFolderPaths],
  );
  const stats = useMemo(
    () => (plan ? summarizeSelectedRun(plan, executableIndices, reversedRows) : null),
    [plan, executableIndices, reversedRows],
  );
  const applyAvailability = useMemo(() => deriveApplyAvailability({
    workspace: selectedCompareWorkspace,
    workspaceExecutionAccess,
    compareActivity: selectedScopeWorkspace?.activity ?? null,
    scopeCalculationPending,
    scopeCalculationFailed,
    executableCount: executableIndices.length,
  }), [
    selectedCompareWorkspace,
    workspaceExecutionAccess,
    selectedScopeWorkspace?.activity,
    scopeCalculationPending,
    scopeCalculationFailed,
    executableIndices.length,
  ]);

  return {
    selectedScopeWorkspace,
    selectedCompareWorkspace,
    selectedCompareWorkspaceKeyRef,
    plan,
    includedRows,
    reversedRows,
    selectedResultTypes,
    searchDraft,
    folderScope,
    appliedAdvancedFilter,
    expandedFolders,
    panelCollapsed,
    sort,
    grouped,
    pathMode,
    collapsedFolderPaths,
    resultView,
    workspaceExecutionAccess,
    reviewEditable,
    workspaceController,
    recordDifferenceViewport,
    searchPending,
    scopeCalculationPending,
    scopeCalculationFailed,
    resultPanelId,
    differencesTabId,
    identicalTabId,
    inScopeIndices,
    executableIndices,
    reviewedRowDecisions,
    reviewKey,
    currentReviewKeyRef,
    currentCompareReviewKey,
    currentCompareReviewKeyRef,
    layout,
    rowPlan,
    folderPathsInLayout,
    anyCollapsed,
    stats,
    applyAvailability,
  };
}
