import { useCallback } from 'react';
import type { Dispatch, MutableRefObject, SetStateAction } from 'react';
import * as applyIpc from '#core/infrastructure/tauri/commands/apply.ts';
import * as operationsIpc from '#core/infrastructure/tauri/commands/operations.ts';
import type { ApplyAvailability } from '#core/application/apply/applyAvailability.ts';
import type { CompareWorkspace } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import {
  directAuthorization,
  INTERACTIVE_APPLY_APPROVAL,
  operationReviewCanSubmit,
  operationReviewPending,
  ownsOperationReviewRequest,
} from '#core/application/operations/operationReview.ts';
import type {
  OperationReviewAction,
  OperationReviewState,
  ReviewRequestFence,
} from '#core/application/operations/operationReview.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { ReviewedRowDecisionDto } from '#core/types/generated/ReviewedRowDecisionDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import type { CompareCompletion } from '../../model/workspacePageModel.ts';

interface ApplyExecutionControllerOptions {
  selectedJob: JobDto | null;
  selectedCompareWorkspace: CompareWorkspace | null;
  plan: PlanDto | null;
  reviewKey: string | null;
  confirmOpen: boolean;
  confirmReviewKey: string | null;
  applyAvailability: ApplyAvailability;
  applyReview: OperationReviewState;
  compareReview: OperationReviewState;
  busy: boolean;
  reviewedRowDecisions: ReviewedRowDecisionDto[];
  selectedTargetIndex: number;
  applyReviewRequestIdRef: MutableRefObject<number>;
  applyReviewRequestRef: MutableRefObject<ReviewRequestFence | null>;
  applyExecutionRequestRef: MutableRefObject<ReviewRequestFence | null>;
  currentReviewKeyRef: MutableRefObject<string | null>;
  confirmReviewKeyRef: MutableRefObject<string | null>;
  compareInFlightRef: MutableRefObject<boolean>;
  autoApplyInFlightRef: MutableRefObject<boolean>;
  compareReviewFetchRequestRef: MutableRefObject<ReviewRequestFence | null>;
  dispatchApplyReview: Dispatch<OperationReviewAction>;
  setConfirmReviewKey: Dispatch<SetStateAction<string | null>>;
  setConfirmOpen: Dispatch<SetStateAction<boolean>>;
  setBusy: Dispatch<SetStateAction<boolean>>;
  setLogReload: Dispatch<SetStateAction<number>>;
  resetConfirmation: () => void;
  resetSafetyUi: () => void;
  doCompare: () => Promise<CompareCompletion | null>;
  refreshLatestRunSummaries: () => void;
  requestResultRestore: (
    job: JobDto,
    targetIndex: number,
    retained: CompareWorkspace | null,
    announce?: boolean,
  ) => void;
  setStatus: StatusApi['setMessage'];
}

export function useApplyExecutionController({
  selectedJob,
  selectedCompareWorkspace,
  plan,
  reviewKey,
  confirmOpen,
  confirmReviewKey,
  applyAvailability,
  applyReview,
  compareReview,
  busy,
  reviewedRowDecisions,
  selectedTargetIndex,
  applyReviewRequestIdRef,
  applyReviewRequestRef,
  applyExecutionRequestRef,
  currentReviewKeyRef,
  confirmReviewKeyRef,
  compareInFlightRef,
  autoApplyInFlightRef,
  compareReviewFetchRequestRef,
  dispatchApplyReview,
  setConfirmReviewKey,
  setConfirmOpen,
  setBusy,
  setLogReload,
  resetConfirmation,
  resetSafetyUi,
  doCompare,
  refreshLatestRunSummaries,
  requestResultRestore,
  setStatus,
}: ApplyExecutionControllerOptions) {
  const openConfirm = useCallback(async () => {
    if (!applyAvailability.available) {
      setStatus(applyAvailability.blockedMessage ?? 'Apply is unavailable', 'err');
      return;
    }
    if (!selectedJob
      || !plan
      || !reviewKey
      || busy
      || compareInFlightRef.current
      || autoApplyInFlightRef.current
      || operationReviewPending(applyReview)
      || applyReviewRequestRef.current
      || compareReviewFetchRequestRef.current
      || compareReview.review) return;
    const request: ReviewRequestFence = {
      key: reviewKey,
      requestId: applyReviewRequestIdRef.current + 1,
    };
    applyReviewRequestIdRef.current = request.requestId;
    applyReviewRequestRef.current = request;
    confirmReviewKeyRef.current = reviewKey;
    setConfirmReviewKey(reviewKey);
    dispatchApplyReview({ type: 'begin', request });
    setConfirmOpen(true);
    try {
      const review = await operationsIpc.reviewApply(plan.owner.identity, reviewedRowDecisions);
      if (!ownsOperationReviewRequest(applyReviewRequestRef.current, request, currentReviewKeyRef.current)) return;
      if (review.status === 'direct_authorized' && !directAuthorization(review)) {
        dispatchApplyReview({
          type: 'failed',
          request,
          error: 'The direct Apply review contradicted its capability report and was rejected',
        });
        return;
      }
      dispatchApplyReview({ type: 'resolved', request, review });
    } catch (error) {
      if (!ownsOperationReviewRequest(applyReviewRequestRef.current, request, currentReviewKeyRef.current)) return;
      dispatchApplyReview({ type: 'failed', request, error: String(error) });
    }
  }, [
    applyAvailability,
    applyReview,
    applyReviewRequestIdRef,
    applyReviewRequestRef,
    autoApplyInFlightRef,
    busy,
    compareInFlightRef,
    compareReview.review,
    compareReviewFetchRequestRef,
    confirmReviewKeyRef,
    currentReviewKeyRef,
    dispatchApplyReview,
    plan,
    reviewKey,
    reviewedRowDecisions,
    selectedJob,
    setConfirmOpen,
    setConfirmReviewKey,
    setStatus,
  ]);

  const doSync = useCallback(async () => {
    if (
      !selectedJob
      || !plan
      || !reviewKey
      || !confirmOpen
      || confirmReviewKey !== reviewKey
      || confirmReviewKeyRef.current !== reviewKey
      || !applyAvailability.available
      || !operationReviewCanSubmit(applyReview)
    ) {
      setStatus('Apply is unavailable until this exact reviewed action set is authorized', 'err');
      return;
    }
    const request = applyReviewRequestRef.current;
    const review = applyReview.review;
    if (!request || !review) return;
    if (busy || autoApplyInFlightRef.current || (applyExecutionRequestRef.current?.requestId === request.requestId
      && applyExecutionRequestRef.current.key === request.key)) return;
    applyExecutionRequestRef.current = request;
    const executionDecisions = reviewedRowDecisions;
    const applyingJob = selectedJob;
    // Whether the progress window stays during a sync is its own Auto-close / When-finished business
    let launchId: number | null = null;
    let executionStarted = false;
    try {
      let authorization = directAuthorization(review);
      if (review.status === 'interactive_apply_confirmation_required') {
        dispatchApplyReview({ type: 'begin_approval', request });
        setStatus('Authorizing this exact apply operation…');
        try {
          authorization = await operationsIpc.approveOperation(
            review.challenge_id,
            INTERACTIVE_APPLY_APPROVAL,
          );
        } catch (error) {
          if (ownsOperationReviewRequest(applyReviewRequestRef.current, request, currentReviewKeyRef.current)) {
            dispatchApplyReview({ type: 'approval_failed', request, error: String(error) });
            setStatus(`Apply authorization failed: ${error}`, 'err');
          }
          return;
        }
        if (!ownsOperationReviewRequest(applyReviewRequestRef.current, request, currentReviewKeyRef.current)) return;
        dispatchApplyReview({ type: 'authorized', request, authorization });
      }
      if (!authorization) throw new Error('the safety review did not authorize this operation');

      resetConfirmation();
      setBusy(true);
      setStatus(`Synchronizing '${applyingJob.name}' (${executionDecisions.length} items)...`);
      // The command returns only after the new window has installed its run-progress listener.
      // Starting apply any earlier loses the phase start/totals on a freshly opened window.
      launchId = await applyIpc.openProgressWindow();
      resetSafetyUi();
      executionStarted = true;
      const applyResult = await applyIpc.applyJob(authorization.authorization_token, launchId);
      setStatus(
        applyResult.cancelled
          ? `Stopped: cancelled after ${applyResult.done} run — re-checking...`
          : `Done: ${applyResult.done} run, ${applyResult.skipped} skipped, ${applyResult.errors} errors — re-checking...`,
        applyResult.errors ? 'err' : 'ok',
      );
      setBusy(false);
      refreshLatestRunSummaries();
      setLogReload((reloadKey) => reloadKey + 1);
      if (applyExecutionRequestRef.current?.requestId === request.requestId
        && applyExecutionRequestRef.current.key === request.key) {
        applyExecutionRequestRef.current = null;
      }
      await doCompare();
    } catch (error) {
      setStatus(
        executionStarted
          ? `Apply failed and may have made partial changes: ${error} — Compare again before continuing`
          : `Apply did not start: ${error} — the reviewed result was retained`,
        'err',
      );
      setBusy(false);
      requestResultRestore(applyingJob, selectedTargetIndex, selectedCompareWorkspace, false);
    } finally {
      if (launchId !== null) {
        void applyIpc.cancelProgressLaunch(launchId).catch((error) => {
          setStatus(`Could not release progress launch ${launchId}: ${error}`, 'err');
        });
      }
      if (applyExecutionRequestRef.current?.requestId === request.requestId
        && applyExecutionRequestRef.current.key === request.key) {
        applyExecutionRequestRef.current = null;
      }
    }
  }, [
    applyAvailability,
    applyExecutionRequestRef,
    applyReview,
    applyReviewRequestRef,
    autoApplyInFlightRef,
    busy,
    confirmOpen,
    confirmReviewKey,
    confirmReviewKeyRef,
    currentReviewKeyRef,
    dispatchApplyReview,
    doCompare,
    plan,
    refreshLatestRunSummaries,
    requestResultRestore,
    resetConfirmation,
    resetSafetyUi,
    reviewKey,
    reviewedRowDecisions,
    selectedCompareWorkspace,
    selectedJob,
    selectedTargetIndex,
    setBusy,
    setLogReload,
    setStatus,
  ]);

  return { openConfirm, doSync };
}
