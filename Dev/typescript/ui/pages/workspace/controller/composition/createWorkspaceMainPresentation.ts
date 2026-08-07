import type { Dispatch } from 'react';
import { countActiveAdvancedFilterGroups } from '#core/domain/compare/runScope.ts';
import type { CompareForgetAvailability } from '#core/application/compare-workspace/compareWorkspaceForget.ts';
import { workspaceHasReviewEdits } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import type { useZoomControl } from '#ui/shared/hooks/useZoomControl.ts';
import type { useAutoScanState } from '../../runtime/autoscan/useAutoScanState.ts';
import type { useAutoScanControls } from '../../runtime/autoscan/useAutoScanControls.ts';
import type { useCompareRunState } from '../../runtime/compare/useCompareRunState.ts';
import type { useCsvExport } from '../../runtime/results/useCsvExport.ts';
import type { useForgetCompareResult } from '../../runtime/results/useForgetCompareResult.ts';
import type { useResultTableActions } from '../../runtime/results/useResultTableActions.ts';
import type { useRootDraftActions } from '../../runtime/roots/useRootDraftActions.ts';
import type { useRootEditorState } from '../../runtime/roots/useRootEditorState.ts';
import type { useRootMutations } from '../../runtime/roots/useRootMutations.ts';
import type { useRunScopeActions } from '../../runtime/results/useRunScopeActions.ts';
import type { useWorkspaceJobState } from '../../runtime/jobs/useWorkspaceJobState.ts';
import type { useWorkspacePanels } from '../../runtime/interaction/useWorkspacePanels.ts';
import type { useWorkspaceResultViewModel } from '../../view-model/useWorkspaceResultViewModel.ts';
import type { useWorkspaceSessionState } from '../../runtime/session/useWorkspaceSessionState.ts';
import type { useWorkspaceSelectionActions } from '../../runtime/session/useWorkspaceSelectionActions.ts';
import { EMPTY_RESULT_TYPES } from '../../model/workspacePageModel.ts';
import type { WorkspaceMainViewProps } from '../../presentation/WorkspaceMainView.tsx';

export interface LogPanelSlotProps {
  jobId: string | null;
  reloadKey: number;
  onClose: () => void;
  onSettings: () => void;
  onStatus: StatusApi['setMessage'];
}

export interface WorkspaceMainPresentation {
  view: Omit<WorkspaceMainViewProps, 'logPanel'>;
  logPanel: LogPanelSlotProps | null;
}

interface WorkspaceMainPresentationOptions {
  autoScan: ReturnType<typeof useAutoScanState>;
  autoScanControls: ReturnType<typeof useAutoScanControls>;
  autoScanVerificationPending: boolean;
  busy: boolean;
  compare: { doCompare: () => Promise<unknown> | void };
  compareRun: ReturnType<typeof useCompareRunState>;
  csv: ReturnType<typeof useCsvExport>;
  dispatchCompare: Dispatch<CompareWorkspaceAction>;
  forget: ReturnType<typeof useForgetCompareResult>;
  forgetAvailability: CompareForgetAvailability;
  jobState: ReturnType<typeof useWorkspaceJobState>;
  openConfirm: () => Promise<unknown> | void;
  panels: ReturnType<typeof useWorkspacePanels>;
  resetSafetyUi: () => void;
  result: ReturnType<typeof useWorkspaceResultViewModel>;
  resultTable: ReturnType<typeof useResultTableActions>;
  reviewBusy: boolean;
  roots: ReturnType<typeof useRootEditorState>;
  rootActions: ReturnType<typeof useRootDraftActions>;
  rootMutations: ReturnType<typeof useRootMutations>;
  runScope: ReturnType<typeof useRunScopeActions>;
  selection: ReturnType<typeof useWorkspaceSelectionActions>;
  session: ReturnType<typeof useWorkspaceSessionState>;
  statusApi: StatusApi;
  zoom: ReturnType<typeof useZoomControl>;
}

/** Maps feature-owned state/actions into the stable main-view component contract. */
export function createWorkspaceMainPresentation({
  autoScan,
  autoScanControls,
  autoScanVerificationPending,
  busy,
  compare,
  compareRun,
  csv,
  dispatchCompare,
  forget,
  forgetAvailability,
  jobState,
  openConfirm,
  panels,
  resetSafetyUi,
  result,
  resultTable,
  reviewBusy,
  roots,
  rootActions,
  rootMutations,
  runScope,
  selection,
  session,
  statusApi,
  zoom,
}: WorkspaceMainPresentationOptions): WorkspaceMainPresentation {
  const {
    anyCollapsed,
    appliedAdvancedFilter,
    applyAvailability,
    collapsedFolderPaths,
    differencesTabId,
    executableIndices,
    expandedFolders,
    folderScope,
    grouped,
    identicalTabId,
    includedRows,
    inScopeIndices,
    layout,
    panelCollapsed,
    pathMode,
    plan,
    recordDifferenceViewport,
    resultPanelId,
    resultView,
    reversedRows,
    reviewEditable,
    rowPlan,
    scopeCalculationFailed,
    scopeCalculationPending,
    searchDraft,
    searchPending,
    selectedCompareWorkspace,
    selectedResultTypes,
    selectedScopeWorkspace,
    sort,
    stats,
    workspaceController,
    workspaceExecutionAccess,
  } = result;
  const hasDifferences = !!plan && plan.ops.length > 0;
  const runScopeProps = plan && hasDifferences && resultView === 'differences' && !compareRun.active
    ? {
        plan,
        reversedRows,
        selectedResultTypes,
        onSelectedResultTypesChange: runScope.changeSelectedResultTypes,
        folderScope,
        onFolderScopeChange: runScope.changeFolderScope,
        collapsed: panelCollapsed,
        expandedFolders,
        onToggleCollapsed: runScope.togglePanel,
        onToggleExpandedFolder: runScope.toggleExpandedFolder,
        onClearRunScope: runScope.clearRunScope,
      }
    : null;
  const identicalResultsProps = selectedCompareWorkspace
    ? {
        workspace: selectedCompareWorkspace.identical,
        onSearchDraftChange: workspaceController.changeIdenticalSearchDraft,
        onLoadMore: workspaceController.loadMoreIdentical,
        onRetry: workspaceController.retryIdentical,
      }
    : null;
  const planTableProps = plan && hasDifferences && selectedCompareWorkspace
    ? {
        plan,
        reversedRows,
        includedRows,
        rowPlan,
        displayOrder: layout.displayOrder,
        inScopeIndices,
        pathMode,
        grouped,
        sort,
        collapsedFolderPaths,
        workspaceKey: selectedCompareWorkspace.key,
        viewport: selectedCompareWorkspace.differences.viewport,
        reviewEditable,
        wrap: panels.resultViewportElement,
        onSetRowIncluded: resultTable.setRowIncluded,
        onSetRowsIncluded: resultTable.setRowsIncluded,
        onToggleRowDirection: resultTable.toggleRowDirection,
        onToggleFolderFold: resultTable.toggleFolderFold,
        onSort: resultTable.toggleSort,
        onContextRow: resultTable.openRowMenu,
        onViewportChange: recordDifferenceViewport,
      }
    : null;

  const view: Omit<WorkspaceMainViewProps, 'logPanel'> = {
    sidebar: {
      jobs: session.jobs,
      currentJobId: session.selectedJob?.job_id ?? null,
      latestRunByJobId: jobState.latestRunByJobId,
      busy,
      reviewing: reviewBusy,
      appVersion: jobState.appVersion,
      jobsDir: jobState.jobsDir,
      onSelect: selection.selectJob,
      onEdit: (name) => { if (!busy) panels.setEditor({ name }); },
      onNew: () => { if (!busy) panels.setEditor({ name: null }); },
    },
    toolbar: {
      job: session.selectedJob,
      hasPlan: !!plan,
      planHeader: plan?.header ?? null,
      executableCount: executableIndices.length,
      stats,
      busy: busy || reviewBusy || autoScanVerificationPending || roots.rootDraftOpen,
      canSync: applyAvailability.available,
      applyBlockedMessage: applyAvailability.blockedMessage,
      autoScanStatus: autoScan.status,
      autoScanControlPending: autoScan.controlPending,
      onCompare: () => { void compare.doCompare(); },
      onSync: () => { void openConfirm(); },
      onToggleLog: () => panels.setLogOpen((value) => !value),
      onToggleAutoScan: autoScanControls.toggle,
    },
    pathLine: {
      job: session.selectedJob,
      rootEditor: roots.selectedRootEditor,
      jobConfiguration: jobState.jobConfiguration,
      peerLink: jobState.jobPeerLink,
      busy,
      reviewing: reviewBusy,
      selectedTargetIndex: session.selectedTargetIndex,
      pathHistory: jobState.pathHistory,
      dropTargetKey: panels.dropTargetKey === 'source' || panels.dropTargetKey === 'target'
        ? panels.dropTargetKey
        : null,
      scopeRef: panels.setPathScope,
      onDraftChange: rootActions.changeRootDraft,
      onSave: (field) => { void rootMutations.saveRootDraft(field); },
      onRevert: rootActions.revertRootDraft,
      onAcceptConflict: rootActions.acceptRootDraftConflict,
      onBrowse: (field) => { void rootActions.browseRoot(field); },
      onSwap: () => { void rootMutations.requestSwap(); },
      onSelectTarget: selection.selectTarget,
      onEditGroup: (group) => {
        if (session.selectedJob && !busy) {
          panels.setEditor({ name: session.selectedJob.name, focusGroup: group });
        }
      },
    },
    candidateNotice: selectedScopeWorkspace?.candidate && selectedCompareWorkspace ? {
      candidate: selectedScopeWorkspace.candidate,
      activeHasReviewEdits: workspaceHasReviewEdits(selectedCompareWorkspace),
      onAdopt: () => {
        const decision = {
          scopeKey: selectedScopeWorkspace.key,
          resultKey: selectedScopeWorkspace.candidate!.workspace.key,
        };
        if (workspaceHasReviewEdits(selectedCompareWorkspace)) panels.setCandidateAdoption(decision);
        else {
          dispatchCompare({
            type: 'candidate_adopted',
            scopeKey: decision.scopeKey,
            expectedResultKey: decision.resultKey,
          });
          resetSafetyUi();
        }
      },
      onDiscard: () => dispatchCompare({
        type: 'candidate_discarded',
        scopeKey: selectedScopeWorkspace.key,
        expectedResultKey: selectedScopeWorkspace.candidate!.workspace.key,
      }),
    } : null,
    activityNotice: selectedCompareWorkspace && selectedScopeWorkspace ? {
      activity: selectedScopeWorkspace.activity,
      workspaceExecutionAccess,
    } : null,
    executionNotice: selectedCompareWorkspace && workspaceExecutionAccess ? {
      access: workspaceExecutionAccess,
      execution: selectedScopeWorkspace?.execution ?? null,
    } : null,
    resultBar: plan && !compareRun.active ? {
      plan,
      resultView,
      onResultViewChange: runScope.changeResultView,
      searchDraft,
      searchPending,
      scopeCalculationPending,
      scopeCalculationFailed,
      onSearchDraftChange: workspaceController.changeDifferenceSearchDraft,
      onClearSearch: () => workspaceController.changeDifferenceSearchDraft(''),
      scope: {
        foundCount: plan.ops.length,
        inScopeCount: inScopeIndices.length,
        selectedCount: executableIndices.length,
        folderScope,
        selectedResultTypes: [...selectedResultTypes],
        advancedFilterCount: countActiveAdvancedFilterGroups(appliedAdvancedFilter),
      },
      onClearScope: runScope.clearRunScope,
      onClearFolderScope: () => runScope.changeFolderScope(null),
      onClearSelectedResultTypes: () => runScope.changeSelectedResultTypes(EMPTY_RESULT_TYPES),
      onClearAdvancedFilters: runScope.clearAdvancedFilters,
      advancedFiltersOpen: !!panels.advancedFiltersAnchor,
      onToggleAdvancedFilters: (anchor) => panels.setAdvancedFiltersAnchor((current) => (current ? null : anchor)),
      exportPending: csv.exportPending,
      onExportCsv: () => { void csv.exportCsv(); },
      forgetAvailable: forgetAvailability.available,
      forgetBlockedMessage: forgetAvailability.available ? null : forgetAvailability.blockedMessage,
      forgetPending: forget.forgetPending,
      onForgetResult: () => {
        if (!selectedScopeWorkspace || !selectedCompareWorkspace) return;
        panels.setForgetRequest({
          scopeKey: selectedScopeWorkspace.key,
          resultKey: selectedCompareWorkspace.key,
        });
      },
      grouped,
      sort,
      anyCollapsed,
      pathMode,
      onToggleFold: runScope.toggleAllFolds,
      onToggleGroup: runScope.toggleGrouping,
      onClearSort: runScope.clearSort,
      onTogglePathMode: runScope.togglePathMode,
      resultPanelId,
      differencesTabId,
      identicalTabId,
    } : null,
    results: {
      plan,
      selectedJobName: session.selectedJob?.name ?? null,
      resultView,
      hasDifferences,
      compareActive: compareRun.active,
      resultPanelId,
      differencesTabId,
      identicalTabId,
      setResultViewportElement: panels.setResultViewportElement,
      runScopeProps,
      comparePanelProps: {
        stages: compareRun.stages,
        cancelling: compareRun.cancelling,
        onCancel: compareRun.cancel,
      },
      identicalResultsProps,
      planTableProps,
    },
    statusBar: {
      status: statusApi.status,
      onAction: statusApi.executeAction,
      onDismissNotice: statusApi.dismissNotice,
      plan,
      inScopeCount: inScopeIndices.length,
      resultView,
      zoom: zoom.zoom,
      onZoomIn: zoom.zoomIn,
      onZoomOut: zoom.zoomOut,
      onZoomReset: zoom.zoomReset,
    },
  };
  const logPanel = panels.logOpen ? {
    jobId: session.selectedJob?.job_id ?? null,
    reloadKey: panels.logReload,
    onClose: () => panels.setLogOpen(false),
    onSettings: () => panels.setSettingsOpen(true),
    onStatus: statusApi.setMessage,
  } : null;
  return { view, logPanel };
}
