// Whether one exact Compare result may be discarded permanently, and the answer at the moment the
// user confirms it. Forget is the only operation that destroys retained evidence, so it fails
// closed: anything that cannot be proven idle here refuses instead of guessing. A view-only or
// expired result stays forgettable — evidence that can no longer be applied is exactly what a user
// wants to clear.

import type { CompareIdentity } from '#core/types/generated/CompareIdentity.ts';
import { sameCompareIdentity } from './compareWorkspaceModel.ts';
import type {
  CompareResultKey,
  CompareScopeKey,
  CompareScopeWorkspace,
  CompareWorkspace,
  CompareWorkspaceRepository,
} from './compareWorkspaceModel.ts';

export type CompareForgetBlockReason =
  | 'no_result'
  | 'not_held_by_scope'
  | 'run_in_progress'
  | 'review_open'
  | 'compare_in_progress'
  | 'restore_in_progress'
  | 'retention_checking';

export type CompareForgetAvailability =
  | { available: true }
  | { available: false; reason: CompareForgetBlockReason; blockedMessage: string };

export interface CompareForgetGate {
  runInFlight: boolean;
  reviewPending: boolean;
}

export interface CompareForgetRequest {
  scopeKey: CompareScopeKey;
  resultKey: CompareResultKey;
}

export type CompareForgetResolution =
  | { status: 'forget'; identity: CompareIdentity }
  | { status: 'refused'; message: string };

const BLOCK_MESSAGE: Record<CompareForgetBlockReason, string> = {
  no_result: 'There is no Compare result to discard',
  not_held_by_scope: 'This result is no longer held by its job and target; reopen it before discarding it',
  run_in_progress: 'A run or AutoScan verification is in progress; wait for it to finish before discarding this evidence',
  review_open: 'An Apply or Compare review is open; close it before discarding this evidence',
  compare_in_progress: 'A Compare is running for this job and target; wait for it to finish',
  restore_in_progress: 'This result is still being restored from the backend',
  retention_checking: 'This result is still being confirmed against the retained backend evidence',
};

function blocked(reason: CompareForgetBlockReason): CompareForgetAvailability {
  return { available: false, reason, blockedMessage: BLOCK_MESSAGE[reason] };
}

export function deriveCompareForgetAvailability(input: {
  scope: CompareScopeWorkspace | null;
  workspace: CompareWorkspace | null;
  runInFlight: boolean;
  reviewPending: boolean;
}): CompareForgetAvailability {
  const { scope, workspace } = input;
  if (!workspace || !scope) return blocked('no_result');
  if (scope.active?.key !== workspace.key && scope.candidate?.workspace.key !== workspace.key) {
    return blocked('not_held_by_scope');
  }
  if (input.runInFlight) return blocked('run_in_progress');
  if (input.reviewPending) return blocked('review_open');
  if (scope.activity.status === 'comparing') return blocked('compare_in_progress');
  if (scope.restoration.status === 'loading') return blocked('restore_in_progress');
  if (workspace.retention.status === 'checking') return blocked('retention_checking');
  return { available: true };
}

/**
 * Re-answers the question at confirmation time. The dialog is modeless with respect to backend
 * events, so the result it was opened for may have been superseded, restored, or replaced while it
 * was open; discarding whatever now sits in that slot would destroy evidence the user never saw.
 */
export function resolveCompareResultForget(
  repository: CompareWorkspaceRepository,
  request: CompareForgetRequest,
  gate: CompareForgetGate,
): CompareForgetResolution {
  const scope = repository.scopes.find((entry) => entry.key === request.scopeKey) ?? null;
  const workspace = scope?.active?.key === request.resultKey
    ? scope.active
    : scope?.candidate?.workspace.key === request.resultKey
      ? scope.candidate.workspace
      : null;
  if (!scope || !workspace) {
    return {
      status: 'refused',
      message: 'This Compare result changed while the confirmation was open; nothing was discarded',
    };
  }
  const availability = deriveCompareForgetAvailability({
    scope,
    workspace,
    runInFlight: gate.runInFlight,
    reviewPending: gate.reviewPending,
  });
  if (!availability.available) return { status: 'refused', message: availability.blockedMessage };
  if (!sameCompareIdentity(workspace.identity, workspace.plan.owner.identity)) {
    return {
      status: 'refused',
      message: 'This Compare result does not agree with its own plan owner; nothing was discarded',
    };
  }
  return { status: 'forget', identity: workspace.identity };
}
