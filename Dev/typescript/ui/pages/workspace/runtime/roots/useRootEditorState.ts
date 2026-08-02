import { useLayoutEffect, useReducer, useRef } from 'react';
import {
  activeRootEditor,
  emptyRootEditorRepository,
  reduceRootEditors,
  rootDraftIsDirty,
} from '#core/application/jobs/rootEditor.ts';
import type { RootEditorKey, RootEditorWorkspace } from '#core/application/jobs/rootEditor.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';

export function useRootEditorState(selectedJob: JobDto | null, selectedTargetIndex: number) {
  const [repository, dispatch] = useReducer(reduceRootEditors, emptyRootEditorRepository);
  const selectedRootEditor = activeRootEditor(repository);
  const rootSaveRequestId = useRef(0);
  const rootPickerRequestId = useRef(0);
  const rootSaveInFlight = useRef<{ workspaceKey: RootEditorKey; requestId: number } | null>(null);
  const liveRootEditor = useRef<RootEditorWorkspace | null>(null);
  liveRootEditor.current = selectedRootEditor;
  const rootDraftOpen = !!selectedRootEditor && (
    rootDraftIsDirty(selectedRootEditor, 'source') || rootDraftIsDirty(selectedRootEditor, 'target')
  );

  useLayoutEffect(() => {
    const target = selectedJob?.targets[selectedTargetIndex];
    if (!selectedJob || target === undefined) {
      dispatch({ type: 'selection_rebound', owner: null, values: { source: '', target: '' } });
      return;
    }
    dispatch({
      type: 'selection_rebound',
      owner: {
        jobId: selectedJob.job_id,
        jobName: selectedJob.name,
        configRevision: selectedJob.config_revision,
        targetIndex: selectedTargetIndex,
      },
      values: { source: selectedJob.source, target },
    });
  }, [selectedJob, selectedTargetIndex]);

  return {
    dispatch,
    liveRootEditor,
    rootDraftOpen,
    rootPickerRequestId,
    rootSaveInFlight,
    rootSaveRequestId,
    selectedRootEditor,
  };
}
