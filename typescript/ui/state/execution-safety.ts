export type RootEditKeyAction = 'commit' | 'revert' | null;

export function rootEditKeyAction(key: string): RootEditKeyAction {
  if (key === 'Enter') return 'commit';
  if (key === 'Escape') return 'revert';
  return null;
}

export interface ExecutionInteractionState {
  busy: boolean;
  editorOpen: boolean;
  settingsOpen: boolean;
  confirmationOpen: boolean;
  candidateAdoptionOpen: boolean;
  rootDraftOpen: boolean;
  rootSwapOpen: boolean;
  contextMenuOpen: boolean;
  reviewPending: boolean;
}

export function interactionBlocksUnattendedWrite(state: ExecutionInteractionState): boolean {
  return state.busy
    || interactionConflictsWithReservedWrite(state);
}

export function interactionConflictsWithReservedWrite(state: ExecutionInteractionState): boolean {
  return state.editorOpen
    || state.settingsOpen
    || state.confirmationOpen
    || state.candidateAdoptionOpen
    || state.rootDraftOpen
    || state.rootSwapOpen
    || state.contextMenuOpen
    || state.reviewPending;
}
