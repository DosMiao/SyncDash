import { useLayoutEffect, useRef } from 'react';
import type { ExecutionInteractionState } from '#core/application/safety/executionSafety.ts';

interface WorkspaceInteractionSnapshotOptions extends ExecutionInteractionState {}

/** A live, render-independent safety snapshot consumed by asynchronous execution gates. */
export function useWorkspaceInteractionSnapshot(options: WorkspaceInteractionSnapshotOptions) {
  const snapshot = useRef<ExecutionInteractionState>(options);

  useLayoutEffect(() => {
    snapshot.current = options;
  }, [
    options.busy,
    options.candidateAdoptionOpen,
    options.confirmationOpen,
    options.contextMenuOpen,
    options.editorOpen,
    options.reviewPending,
    options.rootDraftOpen,
    options.rootSwapOpen,
    options.settingsOpen,
  ]);

  return snapshot;
}
