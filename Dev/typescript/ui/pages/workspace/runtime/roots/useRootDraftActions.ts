import { useCallback } from 'react';
import type { Dispatch, MutableRefObject } from 'react';
import * as ipc from '#core/infrastructure/tauri/commands/main.ts';
import type {
  RootEditorAction,
  RootEditorWorkspace,
  RootField,
} from '#core/application/jobs/rootEditor.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

interface RootDraftActionsOptions {
  liveRootEditorRef: MutableRefObject<RootEditorWorkspace | null>;
  rootPickerRequestIdRef: MutableRefObject<number>;
  dispatchRootEditor: Dispatch<RootEditorAction>;
  pushHistory: (candidatePath: string) => void;
  setStatus: StatusApi['setMessage'];
}

export function useRootDraftActions({
  liveRootEditorRef,
  rootPickerRequestIdRef,
  dispatchRootEditor,
  pushHistory,
  setStatus,
}: RootDraftActionsOptions) {
  const changeRootDraft = useCallback((field: RootField, value: string) => {
    const workspace = liveRootEditorRef.current;
    if (!workspace) return;
    dispatchRootEditor({ type: 'draft_changed', workspaceKey: workspace.key, field, value });
  }, [dispatchRootEditor, liveRootEditorRef]);

  const revertRootDraft = useCallback((field: RootField) => {
    const workspace = liveRootEditorRef.current;
    if (!workspace) return;
    dispatchRootEditor({ type: 'draft_reverted', workspaceKey: workspace.key, field });
  }, [dispatchRootEditor, liveRootEditorRef]);

  const acceptRootDraftConflict = useCallback((field: RootField) => {
    const workspace = liveRootEditorRef.current;
    if (!workspace) return;
    dispatchRootEditor({ type: 'draft_conflict_accepted', workspaceKey: workspace.key, field });
  }, [dispatchRootEditor, liveRootEditorRef]);

  const browseRoot = useCallback(async (field: RootField) => {
    const workspace = liveRootEditorRef.current;
    if (!workspace) return;
    const requestId = rootPickerRequestIdRef.current + 1;
    rootPickerRequestIdRef.current = requestId;
    try {
      const selectedPath = await ipc.pickDirectory({
        title: `Select the ${field} directory`,
        defaultPath: workspace.draft[field].trim() || workspace.committed[field],
      });
      if (!selectedPath) return;
      const currentWorkspace = liveRootEditorRef.current;
      if (rootPickerRequestIdRef.current !== requestId || currentWorkspace?.key !== workspace.key) {
        setStatus('The directory selection was ignored because the selected job changed while the picker was open');
        return;
      }
      dispatchRootEditor({
        type: 'draft_changed',
        workspaceKey: workspace.key,
        field,
        value: selectedPath,
      });
      pushHistory(selectedPath);
      setStatus(`Selected ${field} draft → ${selectedPath}. Choose Save to update the job.`);
    } catch (error) {
      if (rootPickerRequestIdRef.current !== requestId) return;
      setStatus(`Can't open the picker: ${error}`, 'err');
    }
  }, [
    dispatchRootEditor,
    liveRootEditorRef,
    pushHistory,
    rootPickerRequestIdRef,
    setStatus,
  ]);

  return {
    changeRootDraft,
    revertRootDraft,
    acceptRootDraftConflict,
    browseRoot,
  };
}
