// The workspace shell: registry selection, root editing, panel visibility, status, and the
// Compare run's own state.
//
// Order is load-bearing beyond React's rule of hooks: useRootEditorState reads the selection
// produced by useWorkspaceSessionState, and useCompareRunState takes the status setter produced
// by useStatus.

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
  const rootEditorState = useRootEditorState(session.selectedJob, session.selectedTargetIndex);
  const [compareWorkspaceRepository, dispatchCompareWorkspace] = useReducer(
    reduceCompareWorkspaces,
    emptyCompareWorkspaceRepository,
  );
  const [busy, setBusy] = useState(false);
  const autoApplyInFlight = useRef(false);
  const panels = useWorkspacePanels();
  const statusApi = useStatus('');
  const compareRun = useCompareRunState(statusApi.setMessage);

  return {
    session,
    rootEditorState,
    compareWorkspaceRepository,
    dispatchCompareWorkspace,
    busy,
    setBusy,
    autoApplyInFlight,
    panels,
    statusApi,
    compareRun,
  };
}
