import { useCallback, useRef, useState } from 'react';
import type { CompareForgetRequest } from '#core/application/compare-workspace/compareWorkspaceForget.ts';
import type { JobEditorApi } from '#ui/features/jobs/model/jobEditorModel.ts';
import type {
  CandidateAdoption,
  ContextMenuState,
  RootSwapRequest,
} from '../../model/workspacePageModel.ts';

export interface JobEditorIntent {
  name: string | null;
  focusGroup?: string;
}

/**
 * Owns transient workspace UI surfaces and the DOM handles used by root dropping.
 * Domain/run state deliberately stays outside this hook so closing a surface cannot mutate evidence.
 */
export function useWorkspacePanels() {
  const [editor, setEditor] = useState<JobEditorIntent | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [advancedFiltersAnchor, setAdvancedFiltersAnchor] = useState<DOMRect | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [logOpen, setLogOpen] = useState(false);
  const [logReload, setLogReload] = useState(0);
  const [dropTargetKey, setDropTargetKey] = useState<string | null>(null);
  const [resultViewportElement, setResultViewportElement] = useState<HTMLDivElement | null>(null);
  const [candidateAdoption, setCandidateAdoption] = useState<CandidateAdoption | null>(null);
  const [forgetRequest, setForgetRequest] = useState<CompareForgetRequest | null>(null);
  const [askSwap, setAskSwap] = useState<RootSwapRequest | null>(null);

  // The native drag listener is registered once and reads the live droppable regions at drop time.
  const dropScope = useRef<{ editor: HTMLElement | null; path: HTMLElement | null }>({
    editor: null,
    path: null,
  });
  const editorApi = useRef<JobEditorApi | null>(null);

  // Stable ref callbacks avoid a null-detach/reattach cycle on every editor keystroke.
  const setPathScope = useCallback((element: HTMLElement | null) => {
    dropScope.current.path = element;
  }, []);
  const setEditorScope = useCallback((element: HTMLElement | null) => {
    dropScope.current.editor = element;
  }, []);

  const resetTransientPanels = useCallback(() => {
    setAdvancedFiltersAnchor(null);
    setContextMenu(null);
    setAskSwap(null);
    setCandidateAdoption(null);
    setForgetRequest(null);
  }, []);

  return {
    advancedFiltersAnchor,
    askSwap,
    candidateAdoption,
    contextMenu,
    dropScope,
    dropTargetKey,
    editor,
    editorApi,
    forgetRequest,
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
    setForgetRequest,
    setLogOpen,
    setLogReload,
    setPathScope,
    setResultViewportElement,
    setSettingsOpen,
    settingsOpen,
  };
}
