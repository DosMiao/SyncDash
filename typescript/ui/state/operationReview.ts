import type { AuthorizationDto } from '../../core/types/generated/AuthorizationDto';
import type { CompareIdentity } from '../../core/types/generated/CompareIdentity';
import type { OperationApprovalDto } from '../../core/types/generated/OperationApprovalDto';
import type { OperationReviewDto } from '../../core/types/generated/OperationReviewDto';
import type { SelectedRowDto } from '../../core/types/generated/SelectedRowDto';

export interface ReviewRequestFence {
  key: string;
  requestId: number;
}

export type ConfirmationReview = Extract<OperationReviewDto, {
  status: 'compare_confirmation_required' | 'interactive_apply_confirmation_required';
}>;
type ReviewReadyReview = Exclude<OperationReviewDto, { status: 'direct_authorized' }>;
type AuthorizedReview = Exclude<OperationReviewDto, { status: 'blocked' }>;

export type OperationReviewState =
  | {
    phase: 'idle';
    request: null;
    review: null;
    authorization: null;
    error: null;
  }
  | {
    phase: 'reviewing';
    request: ReviewRequestFence;
    review: null;
    authorization: null;
    error: null;
  }
  | {
    phase: 'ready';
    request: ReviewRequestFence;
    review: ReviewReadyReview;
    authorization: null;
    error: null;
  }
  | {
    phase: 'approving';
    request: ReviewRequestFence;
    review: ConfirmationReview;
    authorization: null;
    error: null;
  }
  | {
    phase: 'authorized';
    request: ReviewRequestFence;
    review: AuthorizedReview;
    authorization: AuthorizationDto;
    error: null;
  }
  | {
    phase: 'review_failed';
    request: ReviewRequestFence;
    review: null;
    authorization: null;
    error: string;
  }
  | {
    phase: 'approval_failed';
    request: ReviewRequestFence;
    review: ConfirmationReview;
    authorization: null;
    error: string;
  };

export const INITIAL_OPERATION_REVIEW: OperationReviewState = {
  phase: 'idle',
  request: null,
  review: null,
  authorization: null,
  error: null,
};

export type OperationReviewAction =
  | { type: 'begin'; request: ReviewRequestFence }
  | { type: 'resolved'; request: ReviewRequestFence; review: OperationReviewDto }
  | { type: 'failed'; request: ReviewRequestFence; error: string }
  | { type: 'begin_approval'; request: ReviewRequestFence }
  | { type: 'authorized'; request: ReviewRequestFence; authorization: AuthorizationDto }
  | { type: 'approval_failed'; request: ReviewRequestFence; error: string }
  | { type: 'reset' };

function stateOwnsRequest(state: OperationReviewState, request: ReviewRequestFence): boolean {
  return state.request?.requestId === request.requestId && state.request.key === request.key;
}

export function operationReviewReducer(
  state: OperationReviewState,
  action: OperationReviewAction,
): OperationReviewState {
  if (action.type === 'reset') return INITIAL_OPERATION_REVIEW;
  if (action.type === 'begin') {
    return {
      phase: 'reviewing',
      request: action.request,
      review: null,
      authorization: null,
      error: null,
    };
  }
  if (!stateOwnsRequest(state, action.request)) return state;
  switch (action.type) {
    case 'resolved': {
      if (state.phase !== 'reviewing') return state;
      if (action.review.status === 'direct_authorized') {
        const authorization = directAuthorization(action.review);
        if (!authorization) {
          return {
            phase: 'review_failed',
            request: action.request,
            review: null,
            authorization: null,
            error: 'The direct operation authorization contradicted its capability report',
          };
        }
        return {
          phase: 'authorized',
          request: action.request,
          review: action.review,
          authorization,
          error: null,
        };
      }
      return {
        phase: 'ready',
        request: action.request,
        review: action.review,
        authorization: null,
        error: null,
      };
    }
    case 'failed':
      if (state.phase !== 'reviewing') return state;
      return {
        phase: 'review_failed',
        request: action.request,
        review: null,
        authorization: null,
        error: action.error,
      };
    case 'begin_approval':
      if (state.phase !== 'ready' || !isConfirmationReview(state.review)) return state;
      return {
        phase: 'approving',
        request: action.request,
        review: state.review,
        authorization: null,
        error: null,
      };
    case 'authorized':
      if (state.phase !== 'approving') return state;
      return {
        phase: 'authorized',
        request: action.request,
        review: state.review,
        authorization: action.authorization,
        error: null,
      };
    case 'approval_failed':
      if (state.phase !== 'approving') return state;
      return {
        phase: 'approval_failed',
        request: action.request,
        review: state.review,
        authorization: null,
        error: action.error,
      };
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

export function isConfirmationReview(review: OperationReviewDto): review is ConfirmationReview {
  return review.status === 'compare_confirmation_required'
    || review.status === 'interactive_apply_confirmation_required';
}

export function normalizeApprovalChoices(
  review: ConfirmationReview,
  choices: ApprovalChoices,
): ApprovalChoices {
  const rememberForSession = review.can_remember_for_session && choices.rememberForSession;
  const interactiveApply = review.status === 'interactive_apply_confirmation_required';
  return {
    acknowledgeHealth: interactiveApply && review.requires_health_ack && choices.acknowledgeHealth,
    acceptCapabilities: interactiveApply
      ? review.requires_capability_ack && choices.acceptCapabilities
      : choices.acceptCapabilities,
    rememberForSession,
    allowUnattended: interactiveApply
      && rememberForSession
      && review.can_allow_unattended
      && choices.allowUnattended,
  };
}

export function operationApprovalFromChoices(
  review: ConfirmationReview,
  choices: ApprovalChoices,
): OperationApprovalDto {
  const normalized = normalizeApprovalChoices(review, choices);
  if (review.status === 'compare_confirmation_required') {
    return {
      operation: 'compare',
      accept_capabilities: normalized.acceptCapabilities,
      remember_for_session: normalized.rememberForSession,
    };
  }
  return {
    operation: 'interactive_apply',
    acknowledge_health: normalized.acknowledgeHealth,
    accept_capabilities: normalized.acceptCapabilities,
    session_grant: normalized.allowUnattended
      ? 'allow_auto_apply'
      : normalized.rememberForSession
        ? 'remember_capabilities'
        : 'none',
  };
}

export function reviewAllowsApproval(
  review: OperationReviewDto,
  choices: ApprovalChoices,
): boolean {
  switch (review.status) {
    case 'blocked':
      return false;
    case 'direct_authorized':
      return directAuthorization(review) !== null;
    case 'compare_confirmation_required':
      return !review.capabilities.some((capability) => capability.severity === 'block')
        && choices.acceptCapabilities;
    case 'interactive_apply_confirmation_required':
      if (review.capabilities.some((capability) => capability.severity === 'block')) return false;
      if (review.requires_health_ack && !choices.acknowledgeHealth) return false;
      if (review.requires_capability_ack && !choices.acceptCapabilities) return false;
      if (choices.allowUnattended
        && (!choices.rememberForSession || !review.can_allow_unattended)) return false;
      return true;
    default:
      return false;
  }
}

export function directAuthorization(review: OperationReviewDto): AuthorizationDto | null {
  if (review.status !== 'direct_authorized'
    || review.capabilities.some((capability) => capability.severity === 'block')) return null;
  return review.authorization;
}

export function operationReviewCanSubmit(
  state: OperationReviewState,
  choices: ApprovalChoices,
): boolean {
  if (state.phase === 'authorized') return true;
  return state.phase === 'ready' && reviewAllowsApproval(state.review, choices);
}

export function operationReviewPending(state: OperationReviewState): boolean {
  return state.phase === 'reviewing' || state.phase === 'approving';
}

export function operationReviewFailed(state: OperationReviewState): boolean {
  return state.phase === 'review_failed' || state.phase === 'approval_failed';
}

export function ownsOperationReviewRequest(
  active: ReviewRequestFence | null,
  request: ReviewRequestFence,
  currentKey: string | null,
): boolean {
  return active?.requestId === request.requestId
    && active.key === request.key
    && currentKey === request.key;
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
  compareIdentity: CompareIdentity,
  jobId: string,
  configRevision: string,
  targetIndex: number,
  selected: SelectedRowDto[],
): string {
  return JSON.stringify([
    'apply',
    compareIdentity.compare_run_id,
    compareIdentity.job_id,
    compareIdentity.config_revision,
    compareIdentity.target_index,
    jobId,
    configRevision,
    targetIndex,
    normalizedSelectedDecisions(selected),
  ]);
}
