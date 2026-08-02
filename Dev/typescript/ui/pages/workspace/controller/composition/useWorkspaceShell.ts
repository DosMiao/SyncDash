// The workspace shell: registry selection, root editing, panel visibility, status, and the
// Compare run's own state.
//
// These eight hooks were called in this order at the top of the page controller, followed by
// eighty lines of destructuring. Extracting the *contiguous* run preserves React's call order
// exactly — a custom hook runs its own hooks in sequence at the position it is called — while
// moving the ceremony out of the route composition.
//
// Order is load-bearing beyond React's rule: useRootEditorState reads the selection produced by
// useWorkspaceSessionState, and useCompareRunState takes the status setter produced by useStatus.

import { useReducer, useRef, useState } from 'react';

import { emptyCompareWorkspaceRepository } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import { reduceCompareWorkspaces } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import { useStatus } from '#ui/shared/status/useStatus.ts';

import { useCompareRunState } from '../../runtime/compare/useCompareRunState.ts';
import { useWorkspacePanels } from '../../runtime/interaction/useWorkspacePanels.ts';
import { useRootEditorState } from '../../runtime/roots/useRootEditorState.ts';
import { useWorkspaceSessionState } from '../../runtime/session/useWorkspaceSessionState.ts';

export function useWorkspaceShell() {
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

  return {
    jobs,
    selectedJob,
    refreshJobs,
    setRegistrySelection,
    selectedTargetIndex,
    setSelectedTargetIndex,
    selectionRef,
    dispatchRootEditor,
    liveRootEditor,
    rootDraftOpen,
    rootPickerRequestId,
    rootSaveInFlight,
    rootSaveRequestId,
    selectedRootEditor,
    reduceCompareWorkspaces,
    emptyCompareWorkspaceRepository,
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
    status,
    setStatus,
    setStatusAction,
    runStatusAction,
    dismissStatusNotice,
    compareActive,
    compareActivityRequestId,
    cancelCompare,
    compareCancelling,
    compareInFlight,
    compareRateByPhase,
    restoreRequestId,
    compareRunFloor,
    compareRunId,
    compareRunReady,
    setCompareActive,
    setCompareCancelling,
    setCompareStages,
    compareStages,
    compareWorkspaceRepository,
    dispatchCompareWorkspace,
    busy,
    setBusy,
    session,
    rootEditorState,
    autoApplyInFlight,
    panels,
    statusApi,
    compareRun,
  };
}
