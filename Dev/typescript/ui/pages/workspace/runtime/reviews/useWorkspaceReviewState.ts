import { useCallback, useEffect, useReducer, useRef, useState } from 'react';
import type { Dispatch, SetStateAction } from 'react';
import {
  EMPTY_APPROVAL_CHOICES,
  INITIAL_OPERATION_REVIEW,
  operationReviewReducer,
} from '#core/application/operations/operationReview.ts';
import type {
  ApprovalChoices,
  ReviewRequestFence,
} from '#core/application/operations/operationReview.ts';
import { useOperationReviewExpiry } from '#ui/features/operation-review/hooks/useOperationReviewExpiry.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import type { ContextMenuState } from '../../model/workspacePageModel.ts';

interface WorkspaceReviewStateOptions {
  applyReviewKey: string | null;
  compareReviewKey: string | null;
  reviewEditable: boolean;
  setContextMenu: Dispatch<SetStateAction<ContextMenuState | null>>;
  setStatus: StatusApi['setMessage'];
}

export function useWorkspaceReviewState({
  applyReviewKey,
  compareReviewKey,
  reviewEditable,
  setContextMenu,
  setStatus,
}: WorkspaceReviewStateOptions) {
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [confirmReviewKey, setConfirmReviewKey] = useState<string | null>(null);
  const [applyReview, dispatchApplyReview] = useReducer(operationReviewReducer, INITIAL_OPERATION_REVIEW);
  const [applyChoices, setApplyChoices] = useState<ApprovalChoices>(EMPTY_APPROVAL_CHOICES);
  const applyReviewRequestId = useRef(0);
  const applyReviewRequest = useRef<ReviewRequestFence | null>(null);
  const applyExecutionRequest = useRef<ReviewRequestFence | null>(null);
  const confirmReviewKeyRef = useRef<string | null>(null);

  const [compareReview, dispatchCompareReview] = useReducer(operationReviewReducer, INITIAL_OPERATION_REVIEW);
  const [compareChoices, setCompareChoices] = useState<ApprovalChoices>(EMPTY_APPROVAL_CHOICES);
  const compareReviewRequestId = useRef(0);
  const compareReviewRequest = useRef<ReviewRequestFence | null>(null);
  const compareReviewFetchRequest = useRef<ReviewRequestFence | null>(null);
  const compareApprovalRequest = useRef<ReviewRequestFence | null>(null);

  useOperationReviewExpiry(applyReview, dispatchApplyReview);
  useOperationReviewExpiry(compareReview, dispatchCompareReview);

  const resetConfirmation = useCallback(() => {
    applyReviewRequest.current = null;
    confirmReviewKeyRef.current = null;
    setApplyChoices(EMPTY_APPROVAL_CHOICES);
    setConfirmOpen(false);
    setConfirmReviewKey(null);
    dispatchApplyReview({ type: 'reset' });
  }, []);

  const resetCompareReview = useCallback(() => {
    compareReviewRequest.current = null;
    compareReviewFetchRequest.current = null;
    compareApprovalRequest.current = null;
    setCompareChoices(EMPTY_APPROVAL_CHOICES);
    dispatchCompareReview({ type: 'reset' });
  }, []);

  const previousApplyReviewKey = useRef<string | null>(applyReviewKey);
  useEffect(() => {
    if (previousApplyReviewKey.current === applyReviewKey) return;
    previousApplyReviewKey.current = applyReviewKey;
    if (!confirmOpen) return;
    resetConfirmation();
    setStatus('The reviewed action set changed — open confirmation again', 'err');
  }, [applyReviewKey, confirmOpen, resetConfirmation, setStatus]);

  const previousCompareReviewKey = useRef<string | null>(compareReviewKey);
  useEffect(() => {
    if (previousCompareReviewKey.current === compareReviewKey) return;
    previousCompareReviewKey.current = compareReviewKey;
    resetCompareReview();
  }, [compareReviewKey, resetCompareReview]);

  useEffect(() => {
    if (!reviewEditable) setContextMenu(null);
  }, [reviewEditable, setContextMenu]);

  return {
    applyChoices,
    applyExecutionRequest,
    applyReview,
    applyReviewRequest,
    applyReviewRequestId,
    compareApprovalRequest,
    compareChoices,
    compareReview,
    compareReviewFetchRequest,
    compareReviewRequest,
    compareReviewRequestId,
    confirmOpen,
    confirmReviewKey,
    confirmReviewKeyRef,
    dispatchApplyReview,
    dispatchCompareReview,
    resetCompareReview,
    resetConfirmation,
    setApplyChoices,
    setCompareChoices,
    setConfirmOpen,
    setConfirmReviewKey,
  };
}
