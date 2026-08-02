import { useCallback } from 'react';
import type { Dispatch, MutableRefObject, SetStateAction } from 'react';
import * as compareIpc from '#core/infrastructure/tauri/commands/compare.ts';
import * as operationsIpc from '#core/infrastructure/tauri/commands/operations.ts';
import { monitorOwnsAutoScanTicket } from '#core/application/autoscan/autoscan.ts';
import type { AutoScanTicket } from '#core/application/autoscan/autoscan.ts';
import {
  activeWorkspace,
  compareScopeForJob,
  compareScopeKey,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { CompareWorkspacePreferences } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import {
  exactWorkspaceLookupProblem,
  scopeWorkspaceLookupProblem,
} from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import {
  compareReviewKey,
  directAuthorization,
  EMPTY_APPROVAL_CHOICES,
  normalizeApprovalChoices,
  operationApprovalFromChoices,
  operationReviewCanSubmit,
  operationReviewPending,
  ownsOperationReviewRequest,
} from '#core/application/operations/operationReview.ts';
import type {
  ApprovalChoices,
  OperationReviewAction,
  OperationReviewState,
  ReviewRequestFence,
} from '#core/application/operations/operationReview.ts';
import { compareLaunchBlocked } from '#core/application/safety/executionSafety.ts';
import type { ExecutionInteractionState } from '#core/application/safety/executionSafety.ts';
import type { CompareStage } from '#core/domain/compare/compareProgress.ts';
import type { AutoScanStatusDto } from '#core/types/generated/AutoScanStatusDto.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import {
  snapshotJobIdentity,
} from '../../model/workspacePageModel.ts';
import type {
  CompareActivityRequest,
  CompareCompletion,
  JobIdentitySnapshot,
} from '../../model/workspacePageModel.ts';

interface CompareExecutionControllerOptions {
  selectedJob: JobDto | null;
  busy: boolean;
  editor: { name: string | null; focusGroup?: string } | null;
  compareReview: OperationReviewState;
  applyReview: OperationReviewState;
  compareChoices: ApprovalChoices;
  confirmOpen: boolean;
  selectedTargetIndex: number;
  workspacePreferences: CompareWorkspacePreferences;
  compareActivityRequestId: MutableRefObject<number>;
  restoreRequestId: MutableRefObject<number>;
  selectionRef: MutableRefObject<{ job: JobDto | null; targetIndex: number }>;
  liveInteractionState: MutableRefObject<ExecutionInteractionState>;
  compareInFlight: MutableRefObject<boolean>;
  autoApplyInFlight: MutableRefObject<boolean>;
  autoScanStatusRef: MutableRefObject<AutoScanStatusDto | null>;
  autoScanTicket: MutableRefObject<AutoScanTicket | null>;
  applyExecutionRequest: MutableRefObject<ReviewRequestFence | null>;
  applyReviewRequest: MutableRefObject<ReviewRequestFence | null>;
  compareReviewRequest: MutableRefObject<ReviewRequestFence | null>;
  compareReviewFetchRequest: MutableRefObject<ReviewRequestFence | null>;
  compareReviewRequestId: MutableRefObject<number>;
  currentCompareReviewKeyRef: MutableRefObject<string | null>;
  compareApprovalRequest: MutableRefObject<ReviewRequestFence | null>;
  compareRateByPhase: MutableRefObject<Map<string, {
    timestampMs: number;
    bytesDone: number;
    smoothedRate: number;
  }>>;
  compareRunId: MutableRefObject<number>;
  compareRunFloor: MutableRefObject<number>;
  compareRunReady: MutableRefObject<boolean>;
  dispatchCompareWorkspace: Dispatch<CompareWorkspaceAction>;
  dispatchCompareReview: Dispatch<OperationReviewAction>;
  setCompareChoices: Dispatch<SetStateAction<ApprovalChoices>>;
  setSelectedTargetIndex: Dispatch<SetStateAction<number>>;
  setBusy: Dispatch<SetStateAction<boolean>>;
  setCompareStages: Dispatch<SetStateAction<CompareStage[]>>;
  setCompareCancelling: Dispatch<SetStateAction<boolean>>;
  setCompareActive: Dispatch<SetStateAction<boolean>>;
  refreshJobs: () => Promise<JobDto[]>;
  reconcileWorkspaceJob: (previous: JobIdentitySnapshot, refreshedJob: JobDto | null) => void;
  resetSafetyUi: () => void;
  setStatus: StatusApi['setMessage'];
}

export function useCompareExecutionController(options: CompareExecutionControllerOptions) {
  const {
    selectedJob,
    busy,
    editor,
    compareReview,
    applyReview,
    compareChoices,
    confirmOpen,
    selectedTargetIndex,
    workspacePreferences,
    compareActivityRequestId,
    restoreRequestId,
    selectionRef,
    liveInteractionState,
    compareInFlight,
    autoApplyInFlight,
    autoScanStatusRef,
    autoScanTicket,
    applyExecutionRequest,
    applyReviewRequest,
    compareReviewRequest,
    compareReviewFetchRequest,
    compareReviewRequestId,
    currentCompareReviewKeyRef,
    compareApprovalRequest,
    compareRateByPhase,
    compareRunId,
    compareRunFloor,
    compareRunReady,
    dispatchCompareWorkspace,
    dispatchCompareReview,
    setCompareChoices,
    setSelectedTargetIndex,
    setBusy,
    setCompareStages,
    setCompareCancelling,
    setCompareActive,
    refreshJobs,
    reconcileWorkspaceJob,
    resetSafetyUi,
    setStatus,
  } = options;

  const beginCompareActivity = useCallback((
    comparedJob: JobIdentitySnapshot,
    targetIndex: number,
    origin: { kind: 'interactive' } | { kind: 'auto_scan'; generation: number; ticketId: number },
  ): CompareActivityRequest => {
    const requestId = compareActivityRequestId.current + 1;
    compareActivityRequestId.current = requestId;
    const scope = {
      job_id: comparedJob.jobId,
      target_index: targetIndex,
      config_revision: comparedJob.configRevision,
    };
    dispatchCompareWorkspace({ type: 'compare_activity_started', scope, requestId, origin });
    return { scope, requestId };
  }, []);

  const failCompareActivity = useCallback((activity: CompareActivityRequest, error: string) => {
    dispatchCompareWorkspace({
      type: 'compare_activity_failed',
      scopeKey: compareScopeKey(activity.scope),
      requestId: activity.requestId,
      error,
    });
  }, []);

  const requestResultRestore = useCallback((
    job: JobDto,
    targetIndex: number,
    retained: ReturnType<typeof activeWorkspace>,
    announce = true,
  ) => {
    const requestId = restoreRequestId.current + 1;
    restoreRequestId.current = requestId;
    const scope = compareScopeForJob(job, targetIndex);
    const scopeKey = compareScopeKey(scope);
    const selectionStillOwnsScope = () => {
      const selected = selectionRef.current;
      return !!selected.job
        && compareScopeKey(compareScopeForJob(selected.job, selected.targetIndex)) === scopeKey;
    };
    if (!retained) {
      dispatchCompareWorkspace({ type: 'scope_restore_started', scope, requestId });
      void compareIpc.restoreCompare(job.job_id, targetIndex, job.config_revision).then((lookup) => {
        dispatchCompareWorkspace({
          type: 'scope_restore_completed',
          scopeKey,
          requestId,
          lookup,
          preferences: workspacePreferences,
        });
        const lookupProblem = scopeWorkspaceLookupProblem(scope, lookup);
        if (announce && selectionStillOwnsScope() && lookupProblem) {
          setStatus(`Could not restore '${job.name}': ${lookupProblem}`, 'err');
        } else if (announce && selectionStillOwnsScope()) {
          setStatus(lookup.status === 'found'
            ? `${job.name} · restored ${lookup.workspace.plan.ops.length} compare items`
            : `${job.name} · no retained result — Compare to create one`);
        }
      }).catch(async (error) => {
        dispatchCompareWorkspace({ type: 'scope_restore_failed', scopeKey, requestId, error: String(error) });
        const requestOwnedSelection = selectionStillOwnsScope();
        let refreshFailure: unknown = null;
        try {
          const list = await refreshJobs();
          const refreshed = list.find((candidate) => candidate.job_id === job.job_id) ?? null;
          reconcileWorkspaceJob(snapshotJobIdentity(job), refreshed);
        } catch (refreshError) {
          refreshFailure = refreshError;
        }
        if (announce && requestOwnedSelection) {
          setStatus(
            `Could not restore '${job.name}' result: ${error}`
              + (refreshFailure ? ` · job-list refresh failed: ${refreshFailure}` : ' · refreshed the job registry'),
            'err',
          );
        }
      });
      return;
    }
    dispatchCompareWorkspace({ type: 'scope_touched', scope });
    dispatchCompareWorkspace({ type: 'workspace_lookup_started', workspace: retained, requestId });
    void compareIpc.reconcileCompareWorkspace(retained.identity).then((lookup) => {
      dispatchCompareWorkspace({
        type: 'workspace_lookup_completed',
        resultKey: retained.key,
        requestId,
        lookup,
      });
      if (announce && selectionStillOwnsScope()) {
        const lookupProblem = exactWorkspaceLookupProblem(scope, retained.key, lookup);
        if (lookupProblem) {
          setStatus(`Could not confirm '${job.name}': ${lookupProblem}`, 'err');
        } else if (lookup.status === 'missing') {
          setStatus(`${job.name} · retained result missing — Compare again`, 'err');
        }
      }
    }).catch((error) => {
      dispatchCompareWorkspace({
        type: 'workspace_lookup_failed',
        resultKey: retained.key,
        requestId,
        error: String(error),
      });
      if (announce && selectionStillOwnsScope()) {
        setStatus(`Could not confirm '${job.name}' result: ${error}`, 'err');
      }
    });
  }, [reconcileWorkspaceJob, refreshJobs, setStatus, workspacePreferences]);

  const runAuthorizedCompare = useCallback(async (
    authorizationToken: string,
    comparedJob: JobIdentitySnapshot,
    targetIndex: number,
    autoTicket?: AutoScanTicket,
    prestartedActivity?: CompareActivityRequest,
  ): Promise<CompareCompletion | null> => {
    const interaction = liveInteractionState.current;
    if (compareLaunchBlocked(interaction)
      || compareInFlight.current
      || autoApplyInFlight.current) {
      if (prestartedActivity) failCompareActivity(prestartedActivity, 'Another interaction took ownership before Compare launched');
      return null;
    }
    if (autoTicket && (
      !monitorOwnsAutoScanTicket(autoScanStatusRef.current, autoScanTicket.current, autoTicket)
      || applyExecutionRequest.current !== null
      || applyReviewRequest.current !== null
      || compareReviewRequest.current !== null
      || interaction.confirmationOpen
      || interaction.reviewPending
    )) {
      if (prestartedActivity) failCompareActivity(prestartedActivity, 'The AutoScan ticket lost execution ownership before Compare launched');
      return null;
    }
    if (!autoTicket) autoScanTicket.current = null;
    const showProgress = !autoTicket;
    const activity = prestartedActivity ?? beginCompareActivity(
      comparedJob,
      targetIndex,
      autoTicket
        ? { kind: 'auto_scan', generation: autoTicket.generation, ticketId: autoTicket.ticketId }
        : { kind: 'interactive' },
    );
    compareInFlight.current = true;
    const name = comparedJob.name;
    if (!autoTicket) resetSafetyUi();
    if (!autoTicket) setBusy(true);
    setStatus(`${autoTicket ? 'AutoScan is comparing' : 'Comparing'} '${name}'…`);
    if (showProgress) setCompareStages([]);
    compareRateByPhase.current.clear();
    if (showProgress) setCompareCancelling(false);
    compareRunFloor.current = compareRunId.current;
    compareRunReady.current = false;
    if (showProgress) setCompareActive(true);
    try {
      const snapshot = await compareIpc.compareJob(authorizationToken);
      const comparedPlan = snapshot.plan;
      dispatchCompareWorkspace(autoTicket
        ? {
          type: 'autoscan_compare_published',
          snapshot,
          generation: autoTicket.generation,
          ticketId: autoTicket.ticketId,
          preferences: workspacePreferences,
        }
        : { type: 'manual_compare_published', snapshot, preferences: workspacePreferences });
      // A job file can be edited outside the app while it is open. Compare used the authoritative
      // file, so refresh the list row before deciding whether the returned owner belongs on screen.
      // Snapshot first: refreshJobs may commit the new row (or null) before this continuation runs,
      // and then the ref no longer tells us that the selected job changed underneath this compare.
      const selectedBeforeRefresh = selectionRef.current;
      let refreshedJob: JobDto | null = null;
      let refreshProblem: unknown = null;
      try {
        const list = await refreshJobs();
        refreshedJob = list.find((job) => job.job_id === comparedPlan.owner.identity.job_id) ?? null;
        reconcileWorkspaceJob({
          jobId: comparedPlan.owner.identity.job_id,
          name: comparedPlan.owner.job_name,
          configRevision: comparedPlan.owner.identity.config_revision,
        }, refreshedJob);
      } catch (error) {
        refreshProblem = error;
      }
      const selected = selectionRef.current;
      const selectedJob = selected.job?.job_id === comparedPlan.owner.identity.job_id && !refreshProblem
        ? refreshedJob
        : selected.job;
      if (selectedBeforeRefresh.job?.job_id === comparedPlan.owner.identity.job_id && !refreshProblem) {
        if (!refreshedJob) {
          setSelectedTargetIndex(0);
        } else if (selectedBeforeRefresh.job.config_revision !== refreshedJob.config_revision) {
          resetSafetyUi();
        }
      }
      const resultBelongsToSelection = !!selectedJob
        && comparedPlan.owner.identity.job_id === selectedJob.job_id
        && comparedPlan.owner.identity.config_revision === selectedJob.config_revision
        && comparedPlan.owner.identity.target_index === selected.targetIndex;
      if (resultBelongsToSelection) {
        setStatus(
          comparedPlan.ops.length === 0
            ? 'No differences in the compared scope'
            : `${comparedPlan.ops.length} items · ${comparedPlan.header.conflict_count} conflicts`,
          comparedPlan.header.conflict_count > 0 ? 'err' : '',
        );
      } else if (refreshProblem) {
        setStatus(`Compare finished for '${name}', but the refreshed job identity could not be read: ${refreshProblem}`, 'err');
      } else if (autoTicket) {
        setStatus(
          comparedPlan.ops.length === 0
            ? `AutoScan finished for '${name}' — no differences in the compared scope; result retained`
            : `AutoScan finished for '${name}' — ${comparedPlan.ops.length} differences retained for review`,
          comparedPlan.header.conflict_count > 0 ? 'err' : '',
        );
      } else {
        setStatus(`Compare finished for '${name}', but its job or target changed — the result was not attached to the current view`, 'err');
      }
      dispatchCompareWorkspace({
        type: 'compare_activity_finished',
        scopeKey: compareScopeKey(activity.scope),
        requestId: activity.requestId,
      });
      return { plan: comparedPlan };
    } catch (error) {
      const cancelled = String(error) === 'cancelled';
      let suffix = '';
      let refreshProblem: unknown = null;
      try {
        // Compare reads the job file directly. If that read failed because the file was edited,
        // removed, or had its targets changed outside the app, refresh even on failure so the
        // stale list row cannot trap every subsequent attempt on the same invalid selection.
        const selected = selectionRef.current;
        const list = await refreshJobs();
        const refreshedJob = list.find((job) => job.job_id === comparedJob.jobId) ?? null;
        reconcileWorkspaceJob(comparedJob, refreshedJob);
        if (selected.job?.job_id === comparedJob.jobId) {
          if (!refreshedJob) {
            setSelectedTargetIndex(0);
            suffix = ` · '${name}' is no longer a registered job`;
          } else if (selected.job.config_revision !== refreshedJob.config_revision) {
            resetSafetyUi();
            suffix = ' · refreshed the changed job configuration';
          }
        }
      } catch (refreshError) {
        refreshProblem = refreshError;
      }
      const base = cancelled ? 'Compare cancelled' : `${autoTicket ? 'AutoScan Compare' : 'Compare'} failed: ${error}`;
      if (refreshProblem) suffix = ` · job-list refresh failed: ${refreshProblem}`;
      failCompareActivity(activity, String(error));
      setStatus(`${base}${suffix}`, cancelled && !refreshProblem ? '' : 'err');
      return null;
    } finally {
      if (showProgress) setCompareActive(false);
      if (!autoTicket) setBusy(false);
      compareRunReady.current = false;
      compareInFlight.current = false;
    }
  }, [beginCompareActivity, failCompareActivity, refreshJobs, reconcileWorkspaceJob, resetSafetyUi, setStatus, workspacePreferences]);

  const doCompare = useCallback(async (autoTicket?: AutoScanTicket): Promise<CompareCompletion | null> => {
    if (busy || editor || compareInFlight.current || applyExecutionRequest.current || autoApplyInFlight.current) return null;
    if (!autoTicket && !selectedJob) return null;
    if (!autoTicket && autoScanTicket.current !== null) {
      setStatus('Compare is unavailable while AutoScan owns a verification ticket', 'err');
      return null;
    }
    if (!autoTicket && (applyReviewRequest.current || compareReviewRequest.current)) return null;
    if (!autoTicket && operationReviewPending(compareReview)) return null;
    if (autoTicket && (
      !monitorOwnsAutoScanTicket(autoScanStatusRef.current, autoScanTicket.current, autoTicket)
      || applyReviewRequest.current !== null
      || compareReviewRequest.current !== null
      || confirmOpen
      || operationReviewPending(compareReview)
      || operationReviewPending(applyReview)
    )) return null;

    const comparedJob: JobIdentitySnapshot = autoTicket
      ? { jobId: autoTicket.jobId, name: autoTicket.jobName, configRevision: autoTicket.configRevision }
      : snapshotJobIdentity(selectedJob!);
    const targetIndex = autoTicket?.targetIndex ?? selectedTargetIndex;
    const key = compareReviewKey(comparedJob.jobId, comparedJob.configRevision, targetIndex);

    if (autoTicket) {
      const activity = beginCompareActivity(comparedJob, targetIndex, {
        kind: 'auto_scan',
        generation: autoTicket.generation,
        ticketId: autoTicket.ticketId,
      });
      setStatus(`AutoScan is reviewing Compare authorization for '${comparedJob.name}'…`);
      try {
        const review = await operationsIpc.reviewCompare(comparedJob.jobId, targetIndex, {
          generation: autoTicket.generation,
          ticket_id: autoTicket.ticketId,
        });
        const stillOwned = monitorOwnsAutoScanTicket(
          autoScanStatusRef.current,
          autoScanTicket.current,
          autoTicket,
        );
        if (!stillOwned) {
          failCompareActivity(activity, 'The AutoScan ticket was superseded during authorization review');
          return null;
        }
        const authorization = directAuthorization(review);
        if (!authorization) {
          setStatus(
            review.status === 'direct_authorized'
              ? `AutoScan paused: the direct Compare review for '${comparedJob.name}' was internally inconsistent`
              : `AutoScan paused: Compare requires an exact interactive authorization for '${comparedJob.name}'`,
            'err',
          );
          failCompareActivity(activity, 'Interactive Compare authorization is required');
          return null;
        }
        return runAuthorizedCompare(
          authorization.authorization_token,
          comparedJob,
          targetIndex,
          autoTicket,
          activity,
        );
      } catch (error) {
        failCompareActivity(activity, String(error));
        setStatus(`AutoScan could not review Compare authorization: ${error}`, 'err');
        return null;
      }
    }

    if (compareReviewFetchRequest.current?.key === key) return null;

    const request: ReviewRequestFence = {
      key,
      requestId: compareReviewRequestId.current + 1,
    };
    compareReviewRequestId.current = request.requestId;
    compareReviewRequest.current = request;
    compareReviewFetchRequest.current = request;
    setCompareChoices(EMPTY_APPROVAL_CHOICES);
    dispatchCompareReview({ type: 'begin', request });
    setStatus(`Reviewing Compare authorization for '${comparedJob.name}'…`);
    try {
      const review = await operationsIpc.reviewCompare(comparedJob.jobId, targetIndex);
      if (!ownsOperationReviewRequest(compareReviewRequest.current, request, currentCompareReviewKeyRef.current)) {
        return null;
      }
      if (review.status === 'interactive_apply_confirmation_required') {
        const error = 'The Compare review returned an Apply approval challenge and was rejected';
        dispatchCompareReview({ type: 'failed', request, error });
        setStatus(error, 'err');
        return null;
      }
      const authorization = directAuthorization(review);
      if (review.status === 'direct_authorized' && !authorization) {
        const error = 'The direct Compare review contradicted its capability report and was rejected';
        dispatchCompareReview({ type: 'failed', request, error });
        setStatus(error, 'err');
        return null;
      }
      dispatchCompareReview({ type: 'resolved', request, review });
      if (authorization) {
        return runAuthorizedCompare(
          authorization.authorization_token,
          comparedJob,
          targetIndex,
        );
      }
      if (review.status === 'blocked') {
        setStatus(`Compare is blocked for '${comparedJob.name}' — review the required fixes`, 'err');
      } else if (review.status === 'compare_confirmation_required') {
        setStatus(`Compare requires your approval for '${comparedJob.name}'`);
      }
      return null;
    } catch (error) {
      if (!ownsOperationReviewRequest(compareReviewRequest.current, request, currentCompareReviewKeyRef.current)) {
        return null;
      }
      dispatchCompareReview({ type: 'failed', request, error: String(error) });
      setStatus(`Compare authorization review failed: ${error}`, 'err');
      return null;
    } finally {
      if (compareReviewFetchRequest.current?.requestId === request.requestId
        && compareReviewFetchRequest.current.key === request.key) {
        compareReviewFetchRequest.current = null;
      }
    }
  }, [selectedJob, busy, editor, compareReview, applyReview, confirmOpen, selectedTargetIndex, beginCompareActivity, failCompareActivity, runAuthorizedCompare, setStatus]);

  const approveCompareReview = useCallback(async () => {
    const request = compareReviewRequest.current;
    const review = compareReview.review;
    if (!request || !review || !operationReviewCanSubmit(compareReview, compareChoices)) return;
    if (review.status !== 'compare_confirmation_required') return;
    if (compareApprovalRequest.current?.requestId === request.requestId
      && compareApprovalRequest.current.key === request.key) return;
    compareApprovalRequest.current = request;
    const choices = normalizeApprovalChoices(review, compareChoices);
    dispatchCompareReview({ type: 'begin_approval', request });
    setStatus('Authorizing this exact Compare operation…');
    try {
      const authorization = await operationsIpc.approveOperation(
        review.challenge_id,
        operationApprovalFromChoices(review, choices),
      );
      if (!ownsOperationReviewRequest(compareReviewRequest.current, request, currentCompareReviewKeyRef.current)) return;
      const selected = selectionRef.current;
      if (!selected.job) return;
      dispatchCompareReview({ type: 'authorized', request, authorization });
      await runAuthorizedCompare(
        authorization.authorization_token,
        snapshotJobIdentity(selected.job),
        selected.targetIndex,
      );
    } catch (error) {
      if (!ownsOperationReviewRequest(compareReviewRequest.current, request, currentCompareReviewKeyRef.current)) return;
      dispatchCompareReview({ type: 'approval_failed', request, error: String(error) });
      setStatus(`Compare authorization failed: ${error}`, 'err');
    } finally {
      if (compareApprovalRequest.current?.requestId === request.requestId
        && compareApprovalRequest.current.key === request.key) {
        compareApprovalRequest.current = null;
      }
    }
  }, [compareChoices, compareReview, runAuthorizedCompare, setStatus]);
  return { requestResultRestore, doCompare, approveCompareReview };
}
