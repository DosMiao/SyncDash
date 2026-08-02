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

/**
 * Whether an authorized Compare may launch.
 *
 * This is the full ownership set **minus `reviewPending`**, and the omission is required rather
 * than an oversight: an authorized Compare is launched *by* a review, and that review is still
 * pending while its Compare starts. Including the field would make a reviewed Compare unable to
 * run at all.
 *
 * Unattended launches are a different question — an AutoScan-ticketed Compare must additionally
 * see no pending review, and its caller checks that separately.
 *
 * In-flight guards (`compareInFlight`, `autoApplyInFlight`) stay with the hook: they are mutable
 * runtime ownership, not interaction state.
 */
export function compareLaunchBlocked(state: ExecutionInteractionState): boolean {
  return state.busy
    || state.editorOpen
    || state.settingsOpen
    || state.confirmationOpen
    || state.candidateAdoptionOpen
    || state.rootDraftOpen
    || state.rootSwapOpen
    || state.contextMenuOpen;
}

/**
 * Whether saving an edited root must be refused.
 *
 * This is the full ownership set **minus `rootDraftOpen`**, and again the omission is required:
 * the draft being saved is the open draft, so treating it as a conflicting interaction would make
 * saving a root impossible.
 *
 * `reviewPending` is included here, unlike the Compare gate — nothing about saving a root is
 * authorized by a review, so a pending one is someone else's ownership.
 */
export function rootSaveBlocked(state: ExecutionInteractionState): boolean {
  return state.busy
    || state.editorOpen
    || state.settingsOpen
    || state.confirmationOpen
    || state.candidateAdoptionOpen
    || state.rootSwapOpen
    || state.contextMenuOpen
    || state.reviewPending;
}
