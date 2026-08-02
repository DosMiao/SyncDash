// This orchestration shell owns session selection, compare/apply reviews, and mutating Tauri
// workflows. Result semantics live in core/domain, retained UI state lives in core/application,
// and rendering is grouped by feature below ui/. Execution receives only a one-use authorization
// token; Rust owns the authenticated plan and reconstructs every operation.

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { useStatus } from '#ui/shared/status/useStatus.ts';
import { useZoomControl } from '#ui/shared/hooks/useZoomControl.ts';
import { useInteractionLayer } from '#ui/shared/interaction/useInteractionLayer.tsx';
import { emptyCompareWorkspaceRepository } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import {
  reduceCompareWorkspaces,
} from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import { operationReviewPending } from '#core/application/operations/operationReview.ts';
import { summarizeApplyReview, type ApplyReviewTotals } from '#ui/features/apply-review/model/applyReviewTotals.ts';
import { JobEditorController } from '#ui/features/jobs/controllers/job-editor/JobEditorController.tsx';
import { LogPanelController } from '#ui/features/logs/controllers/LogPanelController.tsx';
import { SettingsSheetController } from '#ui/features/settings/controllers/SettingsSheetController.tsx';
import { useCompareProgressSubscription } from '../runtime/compare/useCompareProgressSubscription.ts';
import { useCompareExecutionStatusSubscription } from '../runtime/compare/useCompareExecutionStatusSubscription.ts';
import { useDesktopRootDrop } from '../runtime/roots/useDesktopRootDrop.ts';
import { useMainCloseBlockedStatus } from '../runtime/lifecycle/useMainCloseBlockedStatus.ts';
import { useAutoScanState } from '../runtime/autoscan/useAutoScanState.ts';
import { useAutoScanControls } from '../runtime/autoscan/useAutoScanControls.ts';
import { useAutoScanTicketLifecycle } from '../runtime/autoscan/useAutoScanTicketLifecycle.ts';
import { useRootMutations } from '../runtime/roots/useRootMutations.ts';
import { useRootDraftActions } from '../runtime/roots/useRootDraftActions.ts';
import { useApplyExecutionController } from '../runtime/apply/useApplyExecutionController.ts';
import { useCompareExecutionController } from '../runtime/compare/useCompareExecutionController.ts';
import {
  WorkspacePagePresentation,
  type WorkspacePagePresentationProps,
} from '../presentation/WorkspacePagePresentation.tsx';
import { useWorkspaceResultViewModel } from '../view-model/useWorkspaceResultViewModel.ts';
import { useCsvExport } from '../runtime/results/useCsvExport.ts';
import { useResultTableActions } from '../runtime/results/useResultTableActions.ts';
import { useRunScopeActions } from '../runtime/results/useRunScopeActions.ts';
import { useJobExcludeMutation } from '../runtime/jobs/useJobExcludeMutation.ts';
import { useWorkspaceJobState } from '../runtime/jobs/useWorkspaceJobState.ts';
import { useWorkspaceReviewState } from '../runtime/reviews/useWorkspaceReviewState.ts';
import { useWorkspacePreferences } from '../runtime/preferences/useWorkspacePreferences.ts';
import { useWorkspaceSessionState } from '../runtime/session/useWorkspaceSessionState.ts';
import { useRootEditorState } from '../runtime/roots/useRootEditorState.ts';
import { useWorkspacePanels } from '../runtime/interaction/useWorkspacePanels.ts';
import { useWorkspaceInteractionSnapshot } from '../runtime/interaction/useWorkspaceInteractionSnapshot.ts';
import { useCompareRunState } from '../runtime/compare/useCompareRunState.ts';
import { useWorkspaceSelectionActions } from '../runtime/session/useWorkspaceSelectionActions.ts';
import { useJobEditorLifecycle } from '../runtime/jobs/useJobEditorLifecycle.ts';
import { createWorkspaceMainPresentation } from './composition/createWorkspaceMainPresentation.ts';
import { createWorkspaceOverlayPresentation } from './composition/createWorkspaceOverlayPresentation.ts';

export function WorkspacePageController() {
  const session = useWorkspaceSessionState();
  const {
    jobs,
    selectedJob,
    refreshJobs,
    setRegistrySelection,
    selectedTargetIndex,
    setSelectedTargetIndex,
    selectionRef,
  } = session;
  const rootEditorState = useRootEditorState(selectedJob, selectedTargetIndex);
  const {
    dispatch: dispatchRootEditor,
    liveRootEditor,
    rootDraftOpen,
    rootPickerRequestId,
    rootSaveInFlight,
    rootSaveRequestId,
    selectedRootEditor,
  } = rootEditorState;

  const [compareWorkspaceRepository, dispatchCompareWorkspace] = useReducer(
    reduceCompareWorkspaces,
    emptyCompareWorkspaceRepository,
  );
  const [busy, setBusy] = useState(false);
  const autoApplyInFlight = useRef(false);
  const panels = useWorkspacePanels();
  const {
    advancedFiltersAnchor,
    askSwap,
    candidateAdoption,
    contextMenu,
    dropScope,
    dropTargetKey,
    editor,
    editorApi,
    logOpen,
    logReload,
    resetTransientPanels,
    resultViewportElement,
    setAdvancedFiltersAnchor,
    setAskSwap,
    setCandidateAdoption,
    setContextMenu,
    setDropTargetKey,
    setEditor,
    setEditorScope,
    setLogOpen,
    setLogReload,
    setPathScope,
    setResultViewportElement,
    setSettingsOpen,
    settingsOpen,
  } = panels;
  const statusApi = useStatus('');
  const {
    status,
    setMessage: setStatus,
    offerAction: setStatusAction,
    executeAction: runStatusAction,
    dismissNotice: dismissStatusNotice,
  } = statusApi;
  const compareRun = useCompareRunState(setStatus);
  const {
    active: compareActive,
    activityRequestId: compareActivityRequestId,
    cancel: cancelCompare,
    cancelling: compareCancelling,
    inFlight: compareInFlight,
    rateByPhase: compareRateByPhase,
    restoreRequestId,
    runFloor: compareRunFloor,
    runId: compareRunId,
    runReady: compareRunReady,
    setActive: setCompareActive,
    setCancelling: setCompareCancelling,
    setStages: setCompareStages,
    stages: compareStages,
  } = compareRun;

  // AutoScan is backend-owned. The webview renders status and handles exact trigger tickets; it
  // never owns the clock or assumes that remaining mounted means the watcher is still alive.
  const autoScan = useAutoScanState();
  const autoScanStatus = autoScan.status;
  const autoScanStatusRef = autoScan.statusRef;
  const autoScanTicket = autoScan.ticketRef;
  const autoScanControlPending = autoScan.controlPending;

  const jobState = useWorkspaceJobState({
    selectedJob,
    refreshJobs,
    dispatchCompare: dispatchCompareWorkspace,
    dispatchRootEditor,
    setStatus,
  });
  const {
    appVersion,
    describeMutationFailure,
    expireDeletedJobState,
    jobConfiguration,
    jobsDir,
    latestRunByJobId,
    pathHistory,
    pushHistory,
    reconcileSavedWorkspaceJob,
    reconcileWorkspaceJob,
    refreshLatestRunSummaries,
    setJobConfiguration,
  } = jobState;
  const {
    preferences: workspacePreferences,
    setPreferences: setWorkspacePreferences,
    persistPreferences: persistWorkspacePreferences,
  } = useWorkspacePreferences(setStatus);
  const reportZoomFailure = useCallback((message: string) => setStatus(message, 'err'), [setStatus]);
  const zoom = useZoomControl(reportZoomFailure);
  const result = useWorkspaceResultViewModel({
    repository: compareWorkspaceRepository,
    selectedJob,
    selectedTargetIndex,
    preferences: workspacePreferences,
    resultViewportElement,
    dispatch: dispatchCompareWorkspace,
    setStatus,
  });
  const {
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
  } = result;

  const reviews = useWorkspaceReviewState({
    applyReviewKey: reviewKey,
    compareReviewKey: currentCompareReviewKey,
    reviewEditable,
    setContextMenu,
    setStatus,
  });
  const {
    applyChoices,
    applyExecutionRequest,
    applyReview,
    applyReviewRequest,
    applyReviewRequestId,
    compareApprovalRequest,
    compareChoices,
    compareReview,
    compareReviewFetchRequest,
    compareReviewRequest,
    compareReviewRequestId,
    confirmOpen,
    confirmReviewKey,
    confirmReviewKeyRef,
    dispatchApplyReview,
    dispatchCompareReview,
    resetCompareReview,
    resetConfirmation,
    setApplyChoices,
    setCompareChoices,
    setConfirmOpen,
    setConfirmReviewKey,
  } = reviews;
  const reviewBusy = operationReviewPending(compareReview) || operationReviewPending(applyReview);
  const liveInteractionState = useWorkspaceInteractionSnapshot({
    busy,
    editorOpen: editor !== null,
    settingsOpen,
    confirmationOpen: confirmOpen,
    candidateAdoptionOpen: candidateAdoption !== null,
    rootDraftOpen,
    rootSwapOpen: askSwap !== null,
    contextMenuOpen: contextMenu !== null,
    reviewPending: reviewBusy,
  });

  const runScopeActions = useRunScopeActions({
    workspace: selectedCompareWorkspace,
    preferences: workspacePreferences,
    panelCollapsed,
    grouped,
    pathMode,
    anyCollapsed,
    folderPathsInLayout,
    dispatch: dispatchCompareWorkspace,
    setPreferences: setWorkspacePreferences,
    persistPreferences: persistWorkspacePreferences,
    setAdvancedFiltersAnchor,
    changeSearchDraft: workspaceController.changeDifferenceSearchDraft,
    applyAdvancedFilter: workspaceController.applyAdvancedFilter,
  });

  const csv = useCsvExport({
    plan,
    workspace: selectedCompareWorkspace,
    selectedWorkspaceKeyRef: selectedCompareWorkspaceKeyRef,
    selectedJob,
    resultView,
    scopeCalculationFailed,
    scopeCalculationPending,
    layout,
    reversedRows,
    includedRows,
    setStatus,
    offerStatusAction: setStatusAction,
  });

  const resetSafetyUi = useCallback(() => {
    resetConfirmation();
    resetCompareReview();
    resetTransientPanels();
  }, [resetCompareReview, resetConfirmation, resetTransientPanels]);

  const autoScanControls = useAutoScanControls({
    runtime: autoScan,
    selectedJob,
    selectedTargetIndex,
    setWorkspaceStatus: setStatus,
  });
  const {
    acceptStatus: acceptAutoScanStatus,
    toggle: toggleAutoScan,
  } = autoScanControls;

  useEffect(() => {
    if (!selectedJob || selectedJob.targets.length === 0 || selectedTargetIndex < selectedJob.targets.length) return;
    setSelectedTargetIndex(0);
    resetSafetyUi();
    setStatus(`'${selectedJob.name}' no longer has target ${selectedTargetIndex + 1}; selected target 1`);
  }, [selectedJob, selectedTargetIndex, resetSafetyUi, setStatus]);

  const compare = useCompareExecutionController({
    selectedJob,
    busy,
    editor,
    compareReview,
    applyReview,
    compareChoices,
    confirmOpen,
    selectedTargetIndex,
    workspacePreferences,
    compareActivityRequestId,
    restoreRequestId,
    selectionRef,
    liveInteractionState,
    compareInFlight,
    autoApplyInFlight,
    autoScanStatusRef,
    autoScanTicket,
    applyExecutionRequest,
    applyReviewRequest,
    compareReviewRequest,
    compareReviewFetchRequest,
    compareReviewRequestId,
    currentCompareReviewKeyRef,
    compareApprovalRequest,
    compareRateByPhase,
    compareRunId,
    compareRunFloor,
    compareRunReady,
    dispatchCompareWorkspace,
    dispatchCompareReview,
    setCompareChoices,
    setSelectedTargetIndex,
    setBusy,
    setCompareStages,
    setCompareCancelling,
    setCompareActive,
    refreshJobs,
    reconcileWorkspaceJob,
    resetSafetyUi,
    setStatus,
  });
  const {
    requestResultRestore,
    doCompare,
    approveCompareReview,
  } = compare;


  const apply = useApplyExecutionController({
    selectedJob,
    selectedCompareWorkspace,
    plan,
    reviewKey,
    confirmOpen,
    confirmReviewKey,
    applyAvailability,
    applyReview,
    compareReview,
    applyChoices,
    busy,
    reviewedRowDecisions,
    selectedTargetIndex,
    applyReviewRequestIdRef: applyReviewRequestId,
    applyReviewRequestRef: applyReviewRequest,
    applyExecutionRequestRef: applyExecutionRequest,
    currentReviewKeyRef,
    confirmReviewKeyRef,
    compareInFlightRef: compareInFlight,
    autoApplyInFlightRef: autoApplyInFlight,
    compareReviewFetchRequestRef: compareReviewFetchRequest,
    dispatchApplyReview,
    setApplyChoices,
    setConfirmReviewKey,
    setConfirmOpen,
    setBusy,
    setLogReload,
    resetConfirmation,
    resetSafetyUi,
    doCompare,
    refreshLatestRunSummaries,
    requestResultRestore,
    setStatus,
  });
  const { openConfirm } = apply;


  const selection = useWorkspaceSelectionActions({
    busy,
    repository: compareWorkspaceRepository,
    selectedJob,
    selectionRef,
    setRegistrySelection,
    setSelectedTargetIndex,
    resetSafetyUi,
    requestResultRestore,
    setStatus,
  });

  const jobEditorLifecycle = useJobEditorLifecycle({
    selectedJob,
    setEditor,
    reconcileSavedWorkspaceJob,
    reconcileWorkspaceJob,
    expireDeletedJobState,
    resetSafetyUi,
    setRegistrySelection,
    setSelectedTargetIndex,
    refreshJobs,
    setJobConfiguration,
    pushHistory,
    setStatus,
  });

  const rootMutations = useRootMutations({
    liveRootEditorRef: liveRootEditor,
    liveInteractionStateRef: liveInteractionState,
    rootSaveRequestIdRef: rootSaveRequestId,
    rootSaveInFlightRef: rootSaveInFlight,
    compareInFlightRef: compareInFlight,
    autoApplyInFlightRef: autoApplyInFlight,
    autoScanStatusRef,
    autoScanTicketRef: autoScanTicket,
    selectionRef,
    selectedBusy: busy,
    dispatchRootEditor,
    setBusy,
    setAskSwap,
    describeMutationFailure,
    reconcileSavedWorkspaceJob,
    resetSafetyUi,
    pushHistory,
    refreshJobs,
    setStatus,
    setStatusAction,
  });


  const addExcludes = useJobExcludeMutation({
    selectedJob,
    describeMutationFailure,
    reconcileSavedWorkspaceJob,
    refreshJobs,
    resetSafetyUi,
    setStatus,
    offerStatusAction: setStatusAction,
  });

  const rootActions = useRootDraftActions({
    liveRootEditorRef: liveRootEditor,
    rootPickerRequestIdRef: rootPickerRequestId,
    dispatchRootEditor,
    pushHistory,
    setStatus,
  });
  const {
    changeRootDraft,
    revertRootDraft,
    acceptRootDraftConflict,
    browseRoot,
  } = rootActions;


  const resultTable = useResultTableActions({
    workspace: selectedCompareWorkspace,
    plan,
    reversedRows,
    inScopeIndices,
    selectedJob,
    reviewEditable,
    sort,
    dispatch: dispatchCompareWorkspace,
    setContextMenu,
    addExcludes,
    setStatus,
  });
  const {
    openRowMenu: rowMenu,
    setRowIncluded,
    setRowsIncluded,
    toggleFolderFold,
    toggleRowDirection,
    toggleSort,
  } = resultTable;

  useCompareProgressSubscription({
    runReady: compareRunReady,
    runFloor: compareRunFloor,
    runId: compareRunId,
    rateByPhase: compareRateByPhase,
    setStages: setCompareStages,
    setStatus,
  });

  useMainCloseBlockedStatus(setStatus);

  useDesktopRootDrop({
    dropScope,
    editorApi,
    setDropTargetKey,
    changeRootDraft,
    pushHistory,
    setStatus,
  });

  useAutoScanTicketLifecycle({
    runtime: autoScan,
    acceptStatus: acceptAutoScanStatus,
    doCompare,
    compareInFlightRef: compareInFlight,
    applyExecutionRequestRef: applyExecutionRequest,
    applyReviewRequestRef: applyReviewRequest,
    compareReviewRequestRef: compareReviewRequest,
    autoApplyInFlightRef: autoApplyInFlight,
    liveInteractionStateRef: liveInteractionState,
    setBusy,
    setLogReload,
    refreshLatestRunSummaries,
    setWorkspaceStatus: setStatus,
  });


  useCompareExecutionStatusSubscription({
    dispatch: dispatchCompareWorkspace,
    setStatus,
  });

  useInteractionLayer({
    kind: 'application',
    handlers: {
      compare: () => { void doCompare(); },
      synchronize: () => { void openConfirm(); },
      zoom_in: zoom.zoomIn,
      zoom_out: zoom.zoomOut,
      zoom_reset: zoom.zoomReset,
    },
  });
  useInteractionLayer({
    active: rootDraftOpen,
    kind: 'auxiliary_panel',
    handlers: {},
  });

  const confirmTotals: ApplyReviewTotals | null = useMemo(
    () => (plan
      ? summarizeApplyReview(plan, executableIndices, reversedRows, includedRows)
      : null),
    [plan, executableIndices, reversedRows, includedRows],
  );
  const mainComposition = createWorkspaceMainPresentation({
    autoScan,
    autoScanControls,
    busy,
    compare,
    compareRun,
    csv,
    dispatchCompare: dispatchCompareWorkspace,
    jobState,
    openConfirm,
    panels,
    resetSafetyUi,
    result,
    resultTable,
    reviewBusy,
    roots: rootEditorState,
    rootActions,
    rootMutations,
    runScope: runScopeActions,
    selection,
    session,
    statusApi,
    zoom,
  });
  const overlayComposition = createWorkspaceOverlayPresentation({
    addExcludes,
    apply,
    busy,
    compare,
    confirmTotals,
    dispatchCompare: dispatchCompareWorkspace,
    editorLifecycle: jobEditorLifecycle,
    panels,
    repository: compareWorkspaceRepository,
    resetSafetyUi,
    result,
    reviews,
    rootMutations,
    session,
    statusApi,
  });
  const presentation: WorkspacePagePresentationProps = {
    main: {
      ...mainComposition.view,
      logPanel: mainComposition.logPanel
        ? <LogPanelController {...mainComposition.logPanel} />
        : null,
    },
    ...overlayComposition.presentation,
    jobEditor: overlayComposition.jobEditor
      ? <JobEditorController {...overlayComposition.jobEditor} />
      : null,
    settings: overlayComposition.settings
      ? <SettingsSheetController {...overlayComposition.settings} />
      : null,
  };

  return <WorkspacePagePresentation {...presentation} />;
}
