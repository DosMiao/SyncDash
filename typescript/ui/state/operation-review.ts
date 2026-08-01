import type { AuthorizationDto } from '../../core/types/generated/AuthorizationDto';
import type { CompareOwner } from '../../core/types/generated/CompareOwner';
import type { OperationReviewDto } from '../../core/types/generated/OperationReviewDto';
import type { SelectedRowDto } from '../../core/types/generated/SelectedRowDto';

export interface OperationReviewTicket {
  key: string;
  generation: number;
}

export type OperationReviewPhase = 'idle' | 'reviewing' | 'ready' | 'approving' | 'authorized' | 'error';

export interface OperationReviewState {
  phase: OperationReviewPhase;
  ticket: OperationReviewTicket | null;
  review: OperationReviewDto | null;
  authorization: AuthorizationDto | null;
  error: string | null;
}

export const INITIAL_OPERATION_REVIEW: OperationReviewState = {
  phase: 'idle',
  ticket: null,
  review: null,
  authorization: null,
  error: null,
};

export type OperationReviewAction =
  | { type: 'begin'; ticket: OperationReviewTicket }
  | { type: 'resolved'; ticket: OperationReviewTicket; review: OperationReviewDto }
  | { type: 'failed'; ticket: OperationReviewTicket; error: string }
  | { type: 'begin_approval'; ticket: OperationReviewTicket }
  | { type: 'authorized'; ticket: OperationReviewTicket; authorization: AuthorizationDto }
  | { type: 'approval_failed'; ticket: OperationReviewTicket; error: string }
  | { type: 'reset' };

function owns(state: OperationReviewState, ticket: OperationReviewTicket): boolean {
  return state.ticket?.generation === ticket.generation && state.ticket.key === ticket.key;
}

export function operationReviewReducer(
  state: OperationReviewState,
  action: OperationReviewAction,
): OperationReviewState {
  if (action.type === 'reset') return INITIAL_OPERATION_REVIEW;
  if (action.type === 'begin') {
    return {
      phase: 'reviewing',
      ticket: action.ticket,
      review: null,
      authorization: null,
      error: null,
    };
  }
  if (!owns(state, action.ticket)) return state;
  switch (action.type) {
    case 'resolved':
      return {
        ...state,
        phase: 'ready',
        review: action.review,
        authorization: action.review.authorization,
        error: null,
      };
    case 'failed':
      return { ...state, phase: 'error', error: action.error };
    case 'begin_approval':
      return { ...state, phase: 'approving', error: null };
    case 'authorized':
      return { ...state, phase: 'authorized', authorization: action.authorization, error: null };
    case 'approval_failed':
      // Challenges are one-use even when approval is rejected. Fail closed and require a fresh
      // review instead of presenting a retry button backed by a consumed challenge.
      return { ...state, phase: 'error', error: action.error };
  }
}

export interface ApprovalChoices {
  acknowledgeHealth: boolean;
  acceptCapabilities: boolean;
  rememberForSession: boolean;
  allowUnattended: boolean;
}

export const EMPTY_APPROVAL_CHOICES: ApprovalChoices = {
  acknowledgeHealth: false,
  acceptCapabilities: false,
  rememberForSession: false,
  allowUnattended: false,
};

export function normalizeApprovalChoices(
  review: OperationReviewDto,
  choices: ApprovalChoices,
): ApprovalChoices {
  const rememberForSession = review.can_remember_for_session && choices.rememberForSession;
  return {
    acknowledgeHealth: review.requires_health_ack && choices.acknowledgeHealth,
    acceptCapabilities: review.requires_capability_ack && choices.acceptCapabilities,
    rememberForSession,
    allowUnattended: rememberForSession
      && review.can_allow_unattended
      && choices.allowUnattended,
  };
}

export function reviewAllowsApproval(
  review: OperationReviewDto | null,
  choices: ApprovalChoices,
): boolean {
  if (!review || review.status === 'blocked') return false;
  if (review.status === 'direct_authorized') return directAuthorization(review) !== null;
  if (review.status !== 'confirmation_required') return false;
  if (review.authorization !== null
    || review.blockers.length > 0
    || review.capabilities.some((capability) => capability.severity === 'block')) return false;
  if (!review.challenge_id) return false;
  if (review.requires_health_ack && !choices.acknowledgeHealth) return false;
  if (review.requires_capability_ack && !choices.acceptCapabilities) return false;
  if (choices.allowUnattended && (!choices.rememberForSession || !review.can_allow_unattended)) return false;
  return true;
}

export function directAuthorization(review: OperationReviewDto): AuthorizationDto | null {
  if (review.status !== 'direct_authorized'
    || !review.authorization
    || review.challenge_id !== null
    || review.blockers.length > 0
    || review.capabilities.some((capability) => capability.severity === 'block')
    || review.requires_health_ack
    || review.requires_capability_ack) return null;
  return review.authorization;
}

export function operationReviewCanSubmit(
  state: OperationReviewState,
  choices: ApprovalChoices,
): boolean {
  return state.phase === 'ready' && reviewAllowsApproval(state.review, choices);
}

export function operationReviewPending(state: OperationReviewState): boolean {
  return state.phase === 'reviewing' || state.phase === 'approving';
}

export function ownsOperationReviewTicket(
  active: OperationReviewTicket | null,
  ticket: OperationReviewTicket,
  currentKey: string | null,
): boolean {
  return active?.generation === ticket.generation
    && active.key === ticket.key
    && currentKey === ticket.key;
}

export function compareReviewKey(jobId: string, configRevision: string, targetIndex: number): string {
  return JSON.stringify(['compare', jobId, configRevision, targetIndex]);
}

export function normalizedSelectedDecisions(selected: SelectedRowDto[]): Array<[number, boolean]> {
  const decisions = selected.map((row) => [row.index, row.flipped] as [number, boolean]);
  decisions.sort((left, right) => left[0] - right[0]);
  for (let index = 1; index < decisions.length; index += 1) {
    if (decisions[index - 1]![0] === decisions[index]![0]) {
      throw new Error(`duplicate selected row index ${decisions[index]![0]}`);
    }
  }
  return decisions;
}

export function applyReviewKey(
  owner: CompareOwner,
  jobId: string,
  configRevision: string,
  targetIndex: number,
  selected: SelectedRowDto[],
): string {
  return JSON.stringify([
    'apply',
    owner.identity.compare_run_id,
    owner.identity.job_id,
    owner.identity.config_revision,
    owner.identity.target_index,
    jobId,
    configRevision,
    targetIndex,
    normalizedSelectedDecisions(selected),
  ]);
}
