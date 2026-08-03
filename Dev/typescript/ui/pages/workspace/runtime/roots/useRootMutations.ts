import type { JobRootMutationDto } from '#core/types/generated/JobRootMutationDto.ts';
import type { JobSaveDto } from '#core/types/generated/JobSaveDto.ts';
import { useCallback } from 'react';
import type { Dispatch, MutableRefObject, SetStateAction } from 'react';
import * as jobsIpc from '#core/infrastructure/tauri/commands/jobs.ts';
import { rootDraftIsDirty } from '#core/application/jobs/rootEditor.ts';
import type {
  RootEditorAction,
  RootEditorKey,
  RootEditorWorkspace,
  RootField,
} from '#core/application/jobs/rootEditor.ts';
import { rootSaveBlocked } from '#core/application/safety/executionSafety.ts';
import type { ExecutionInteractionState } from '#core/application/safety/executionSafety.ts';
import type { AutoScanStatusDto } from '#core/types/generated/AutoScanStatusDto.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import {
  rootMutationState,
  statusDeliveryWarning,
} from '../../model/workspacePageModel.ts';
import type {
  JobIdentitySnapshot,
  RootSwapRequest,
} from '../../model/workspacePageModel.ts';
import type { JobListRefreshOutcome } from '../jobs/useWorkspaceJobState.ts';

interface RootMutationsOptions {
  liveRootEditorRef: MutableRefObject<RootEditorWorkspace | null>;
  liveInteractionStateRef: MutableRefObject<ExecutionInteractionState>;
  rootSaveRequestIdRef: MutableRefObject<number>;
  rootSaveInFlightRef: MutableRefObject<{
    workspaceKey: RootEditorKey;
    requestId: number;
  } | null>;
  compareInFlightRef: MutableRefObject<boolean>;
  autoApplyInFlightRef: MutableRefObject<boolean>;
  autoScanStatusRef: MutableRefObject<AutoScanStatusDto | null>;
  autoScanTicketRef: MutableRefObject<unknown | null>;
  selectionRef: MutableRefObject<{ job: JobDto | null; targetIndex: number }>;
  selectedBusy: boolean;
  dispatchRootEditor: Dispatch<RootEditorAction>;
  setBusy: Dispatch<SetStateAction<boolean>>;
  setAskSwap: Dispatch<SetStateAction<RootSwapRequest | null>>;
  describeMutationFailure: (action: string, error: unknown) => Promise<string>;
  reconcileSavedWorkspaceJob: (
    saved: JobSaveDto,
    previous: JobIdentitySnapshot | null,
  ) => void;
  resetSafetyUi: () => void;
  pushHistory: (candidatePath: string) => void;
  refreshJobsForAnnouncement: () => Promise<JobListRefreshOutcome>;
  setStatus: StatusApi['setMessage'];
  setStatusAction: StatusApi['offerAction'];
}

/**
 * A root mutation that already landed, together with everything its reversal needs. Undo is not a
 * plain retry of the forward call: it rebinds the same editor workspace, reconciles retained
 * evidence against the identity the *forward* mutation produced, and resets the safety UI again,
 * so those inputs have to travel with the committed result rather than be rediscovered.
 */
interface CommittedRootMutation {
  result: JobRootMutationDto;
  workspaceKey: RootEditorKey;
  targetIndex: number;
  restore: () => Promise<JobRootMutationDto>;
  restoreFailureAction: string;
  restoredMessage: (restored: JobRootMutationDto) => string;
  successMessage: string;
  undoLabel: string;
}

export function useRootMutations({
  liveRootEditorRef,
  liveInteractionStateRef,
  rootSaveRequestIdRef,
  rootSaveInFlightRef,
  compareInFlightRef,
  autoApplyInFlightRef,
  autoScanStatusRef,
  autoScanTicketRef,
  selectionRef,
  selectedBusy,
  dispatchRootEditor,
  setBusy,
  setAskSwap,
  describeMutationFailure,
  reconcileSavedWorkspaceJob,
  resetSafetyUi,
  pushHistory,
  refreshJobsForAnnouncement,
  setStatus,
  setStatusAction,
}: RootMutationsOptions) {
  const announceRootMutation = useCallback(async (commit: CommittedRootMutation) => {
    const undo = async () => {
      let restored: JobRootMutationDto;
      try {
        restored = await commit.restore();
      } catch (error) {
        throw new Error(await describeMutationFailure(commit.restoreFailureAction, error));
      }
      const restoredState = rootMutationState(restored, commit.targetIndex);
      dispatchRootEditor({
        type: 'workspace_rebound',
        workspaceKey: commit.workspaceKey,
        owner: restoredState.owner,
        values: restoredState.values,
      });
      reconcileSavedWorkspaceJob(restored.mutation, {
        jobId: commit.result.mutation.job_id,
        name: commit.result.mutation.name,
        configRevision: commit.result.mutation.config_revision,
      });
      resetSafetyUi();
      const restoredWarning = statusDeliveryWarning(restored.mutation);
      const restoredRefresh = await refreshJobsForAnnouncement();
      setStatus(
        `${commit.restoredMessage(restored)}${restoredWarning}${restoredRefresh.suffix}`,
        restoredWarning || restoredRefresh.failed ? 'err' : '',
      );
    };
    const warning = statusDeliveryWarning(commit.result.mutation);
    const refresh = await refreshJobsForAnnouncement();
    setStatusAction(
      `${commit.successMessage}${warning}${refresh.suffix}`,
      commit.undoLabel,
      undo,
      warning || refresh.failed ? 'err' : '',
    );
  }, [
    describeMutationFailure,
    dispatchRootEditor,
    reconcileSavedWorkspaceJob,
    refreshJobsForAnnouncement,
    resetSafetyUi,
    setStatus,
    setStatusAction,
  ]);

  const saveRootDraft = useCallback(async (field: RootField) => {
    const workspace = liveRootEditorRef.current;
    const interaction = liveInteractionStateRef.current;
    if (!workspace
      || !rootDraftIsDirty(workspace, field)
      || workspace.save.status === 'saving'
      || rootSaveInFlightRef.current !== null) return;
    if (workspace.conflicts[field]) {
      setStatus(`The saved ${field} changed — choose Keep draft or Cancel before saving`, 'err');
      return;
    }
    if (rootSaveBlocked(interaction)
      || compareInFlightRef.current
      || autoApplyInFlightRef.current
      || (autoScanStatusRef.current?.active_ticket ?? null) !== null
      || autoScanTicketRef.current !== null) {
      setStatus(`Cannot save ${field} while another review or execution owns the job`, 'err');
      return;
    }
    const value = workspace.draft[field].trim();
    if (!value) {
      setStatus(`${field === 'source' ? 'Source' : 'Target'} cannot be empty`, 'err');
      return;
    }
    const requestId = rootSaveRequestIdRef.current + 1;
    rootSaveRequestIdRef.current = requestId;
    const workspaceKey = workspace.key;
    const owner = workspace.owner;
    const before = workspace.committed[field];
    rootSaveInFlightRef.current = { workspaceKey, requestId };
    dispatchRootEditor({ type: 'save_started', workspaceKey, requestId, field });
    let result: JobRootMutationDto;
    try {
      result = await jobsIpc.updateJobRoot(
        owner.jobName,
        owner.jobId,
        owner.configRevision,
        owner.targetIndex,
        field,
        value,
      );
    } catch (error) {
      dispatchRootEditor({ type: 'save_failed', workspaceKey, requestId, error: String(error) });
      setStatus(await describeMutationFailure(
        `Could not save ${field}; the draft was retained`,
        error,
      ), 'err');
      return;
    } finally {
      if (rootSaveInFlightRef.current?.workspaceKey === workspaceKey
        && rootSaveInFlightRef.current.requestId === requestId) {
        rootSaveInFlightRef.current = null;
      }
    }
    const committed = rootMutationState(result, owner.targetIndex);
    dispatchRootEditor({
      type: 'save_committed',
      workspaceKey,
      requestId,
      owner: committed.owner,
      values: committed.values,
    });
    reconcileSavedWorkspaceJob(result.mutation, {
      jobId: owner.jobId,
      name: owner.jobName,
      configRevision: owner.configRevision,
    });
    resetSafetyUi();
    pushHistory(value);
    await announceRootMutation({
      result,
      workspaceKey,
      targetIndex: owner.targetIndex,
      restore: () => jobsIpc.updateJobRoot(
        result.mutation.name,
        result.mutation.job_id,
        result.mutation.config_revision,
        owner.targetIndex,
        field,
        before,
      ),
      restoreFailureAction: `Could not restore ${field}`,
      restoredMessage: () => `Restored ${field}`,
      successMessage: `Changed ${field} → ${value} — Compare again (Ctrl+R)`,
      undoLabel: `Undo ${field} change`,
    });
  }, [
    announceRootMutation,
    autoApplyInFlightRef,
    autoScanStatusRef,
    autoScanTicketRef,
    compareInFlightRef,
    describeMutationFailure,
    dispatchRootEditor,
    liveInteractionStateRef,
    liveRootEditorRef,
    pushHistory,
    reconcileSavedWorkspaceJob,
    resetSafetyUi,
    rootSaveInFlightRef,
    rootSaveRequestIdRef,
    setStatus,
  ]);

  const requestSwap = useCallback(() => {
    const workspace = liveRootEditorRef.current;
    const selectedJob = selectionRef.current.job;
    if (!workspace
      || !selectedJob
      || selectedBusy
      || compareInFlightRef.current
      || autoApplyInFlightRef.current
      || (autoScanStatusRef.current?.active_ticket ?? null) !== null
      || autoScanTicketRef.current !== null) return;
    if (rootDraftIsDirty(workspace, 'source') || rootDraftIsDirty(workspace, 'target')) {
      setStatus('Save or cancel the root drafts before swapping', 'err');
      return;
    }
    if (workspace.owner.jobId !== selectedJob.job_id
      || workspace.owner.configRevision !== selectedJob.config_revision
      || workspace.owner.targetIndex !== selectionRef.current.targetIndex) {
      setStatus('The selected root editor is stale; wait for the job registry to finish refreshing', 'err');
      return;
    }
    setAskSwap({
      workspaceKey: workspace.key,
      owner: workspace.owner,
      values: workspace.committed,
      mode: selectedJob.mode,
    });
  }, [
    autoApplyInFlightRef,
    autoScanStatusRef,
    autoScanTicketRef,
    compareInFlightRef,
    liveRootEditorRef,
    selectionRef,
    selectedBusy,
    setAskSwap,
    setStatus,
  ]);

  const doSwap = useCallback(async (request: RootSwapRequest) => {
    setBusy(true);
    let result: JobRootMutationDto;
    try {
      result = await jobsIpc.swapJobRoots(
        request.owner.jobName,
        request.owner.jobId,
        request.owner.configRevision,
        request.owner.targetIndex,
      );
    } catch (error) {
      setBusy(false);
      setStatus(await describeMutationFailure('Root swap failed', error), 'err');
      return;
    }
    const committed = rootMutationState(result, request.owner.targetIndex);
    dispatchRootEditor({
      type: 'workspace_rebound',
      workspaceKey: request.workspaceKey,
      owner: committed.owner,
      values: committed.values,
    });
    reconcileSavedWorkspaceJob(result.mutation, {
      jobId: request.owner.jobId,
      name: request.owner.jobName,
      configRevision: request.owner.configRevision,
    });
    resetSafetyUi();
    pushHistory(committed.values.source);
    pushHistory(committed.values.target);
    setBusy(false);
    await announceRootMutation({
      result,
      workspaceKey: request.workspaceKey,
      targetIndex: request.owner.targetIndex,
      restore: () => jobsIpc.swapJobRoots(
        result.mutation.name,
        result.mutation.job_id,
        result.mutation.config_revision,
        request.owner.targetIndex,
      ),
      restoreFailureAction: 'Could not undo the root swap',
      restoredMessage: (restored) => `Restored the two roots of '${restored.mutation.name}'`,
      successMessage: `Swapped target ${request.owner.targetIndex + 1} and source for '${result.mutation.name}' — Compare again (Ctrl+R)`,
      undoLabel: 'Undo swap',
    });
  }, [
    announceRootMutation,
    describeMutationFailure,
    dispatchRootEditor,
    pushHistory,
    reconcileSavedWorkspaceJob,
    resetSafetyUi,
    setBusy,
    setStatus,
  ]);

  return { saveRootDraft, requestSwap, doSwap };
}
