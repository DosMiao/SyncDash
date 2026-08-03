import type { AuthorizationDto } from '#core/types/generated/AuthorizationDto.ts';
import type { CompareIdentity } from '#core/types/generated/CompareIdentity.ts';
import type { OperationApprovalDto } from '#core/types/generated/OperationApprovalDto.ts';
import type { OperationReviewDto } from '#core/types/generated/OperationReviewDto.ts';
import type { ReviewedRowDecisionDto } from '#core/types/generated/ReviewedRowDecisionDto.ts';

export interface ReviewRequestFence {
  key: string;
  requestId: number;
}

export type ConfirmationReview = Extract<OperationReviewDto, {
  status: 'interactive_apply_confirmation_required';
}>;
type ReviewReadyReview = Exclude<OperationReviewDto, { status: 'direct_authorized' }>;
type AuthorizedReview = Exclude<OperationReviewDto, { status: 'blocked' }>;

export const OPERATION_REVIEW_EXPIRY_SAFETY_MS = 1_000;

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
  }
  | {
    phase: 'expired';
    request: ReviewRequestFence;
    review: OperationReviewDto;
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
  | { type: 'expired'; request: ReviewRequestFence; error: string }
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
  if (action.type === 'expired') {
    if (!state.review) return state;
    return {
      phase: 'expired',
      request: action.request,
      review: state.review,
      authorization: null,
      error: action.error,
    };
  }
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

export function isConfirmationReview(review: OperationReviewDto): review is ConfirmationReview {
  return review.status === 'interactive_apply_confirmation_required';
}

/**
 * The approval carries no conditions: the review panel presents evidence, not choices. It names the
 * one challenge kind it answers, mirroring `ReviewApproval::InteractiveApply` on the Rust side. The
 * challenge id passed alongside it is what binds a token to the exact reviewed plan.
 */
export const INTERACTIVE_APPLY_APPROVAL: OperationApprovalDto = { operation: 'interactive_apply' };

/**
 * Nothing in the review panel withholds approval any more. The capability list and the plan-share
 * warnings are there so the operator knows what this run will do; a review that reached the
 * confirmation stage at all is one the server is willing to authorize.
 */
export function reviewAllowsApproval(review: OperationReviewDto): boolean {
  switch (review.status) {
    case 'blocked':
      return false;
    case 'direct_authorized':
      return directAuthorization(review) !== null;
    case 'interactive_apply_confirmation_required':
      return true;
    default:
      return false;
  }
}

export function directAuthorization(review: OperationReviewDto): AuthorizationDto | null {
  return review.status === 'direct_authorized' ? review.authorization : null;
}

export function operationReviewCanSubmit(
  state: OperationReviewState,
  nowMs = Date.now(),
): boolean {
  const expiresAtMs = operationReviewExpiresAtMs(state);
  if (expiresAtMs !== null && nowMs + OPERATION_REVIEW_EXPIRY_SAFETY_MS >= expiresAtMs) {
    return false;
  }
  if (state.phase === 'authorized') return true;
  return state.phase === 'ready' && reviewAllowsApproval(state.review);
}

export function operationReviewExpiresAtMs(state: OperationReviewState): number | null {
  if (state.phase === 'authorized') return state.authorization.expires_at_ms;
  if (!state.review || !isConfirmationReview(state.review)) return null;
  return state.review.expires_at_ms;
}

export function operationReviewPending(state: OperationReviewState): boolean {
  return state.phase === 'reviewing' || state.phase === 'approving';
}

export function operationReviewFailed(state: OperationReviewState): boolean {
  return state.phase === 'review_failed' || state.phase === 'approval_failed';
}

export function operationReviewExpired(state: OperationReviewState): boolean {
  return state.phase === 'expired';
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

export function normalizedReviewedRowDecisions(
  reviewedRowDecisions: ReviewedRowDecisionDto[],
): Array<[number, boolean]> {
  const decisions = reviewedRowDecisions.map(
    (decision) => [decision.index, decision.direction_reversed] as [number, boolean],
  );
  decisions.sort((left, right) => left[0] - right[0]);
  for (let index = 1; index < decisions.length; index += 1) {
    if (decisions[index - 1]![0] === decisions[index]![0]) {
      throw new Error(`duplicate reviewed row index ${decisions[index]![0]}`);
    }
  }
  return decisions;
}

export function applyReviewKey(
  compareIdentity: CompareIdentity,
  jobId: string,
  configRevision: string,
  targetIndex: number,
  verificationEpoch: number,
  reviewedRowDecisions: ReviewedRowDecisionDto[],
): string {
  return JSON.stringify([
    'apply',
    compareIdentity.result_id,
    compareIdentity.compare_run_id,
    compareIdentity.job_id,
    compareIdentity.config_revision,
    compareIdentity.target_index,
    jobId,
    configRevision,
    targetIndex,
    verificationEpoch,
    normalizedReviewedRowDecisions(reviewedRowDecisions),
  ]);
}
