import { useEffect } from 'react';

import {
  OPERATION_REVIEW_EXPIRY_SAFETY_MS,
  operationReviewExpiresAtMs,
} from '../state/operationReview.ts';
import type {
  OperationReviewAction,
  OperationReviewState,
} from '../state/operationReview.ts';

export function useOperationReviewExpiry(
  state: OperationReviewState,
  dispatch: React.Dispatch<OperationReviewAction>,
): void {
  useEffect(() => {
    if (!state.request || state.phase === 'expired') return undefined;
    const expiresAtMs = operationReviewExpiresAtMs(state);
    if (expiresAtMs === null) return undefined;
    const request = state.request;
    const delayMs = Math.max(0, expiresAtMs - Date.now() - OPERATION_REVIEW_EXPIRY_SAFETY_MS);
    const timeout = window.setTimeout(() => {
      dispatch({
        type: 'expired',
        request,
        error: 'This authorization window expired. Review the current operation again.',
      });
    }, delayMs);
    return () => window.clearTimeout(timeout);
  }, [dispatch, state]);
}
