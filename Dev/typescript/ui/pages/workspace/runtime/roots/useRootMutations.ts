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
  describeMutationFailure: (name: string, action: string, error: unknown) => Promise<string>;
  reconcileSavedWorkspaceJob: (
    saved: JobSaveDto,
    previous: JobIdentitySnapshot | null,
  ) => void;
  resetSafetyUi: () => void;
  pushHistory: (candidatePath: string) => void;
  refreshJobs: () => Promise<JobDto[]>;
  setStatus: StatusApi['setMessage'];
  setStatusAction: StatusApi['offerAction'];
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
  refreshJobs,
  setStatus,
  setStatusAction,
}: RootMutationsOptions) {
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
        owner.jobName,
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
    const undo = async () => {
      let restored: JobRootMutationDto;
      try {
        restored = await jobsIpc.updateJobRoot(
          result.mutation.name,
          result.mutation.job_id,
          result.mutation.config_revision,
          owner.targetIndex,
          field,
          before,
        );
      } catch (error) {
        throw new Error(await describeMutationFailure(
          result.mutation.name,
          `Could not restore ${field}`,
          error,
        ));
      }
      const restoredState = rootMutationState(restored, owner.targetIndex);
      dispatchRootEditor({
        type: 'workspace_rebound',
        workspaceKey,
        owner: restoredState.owner,
        values: restoredState.values,
      });
      reconcileSavedWorkspaceJob(restored.mutation, {
        jobId: result.mutation.job_id,
        name: result.mutation.name,
        configRevision: result.mutation.config_revision,
      });
      resetSafetyUi();
      const warning = statusDeliveryWarning(restored.mutation);
      try {
        await refreshJobs();
        setStatus(`Restored ${field}${warning}`, warning ? 'err' : '');
      } catch (error) {
        setStatus(`Restored ${field}${warning} · job-list refresh failed: ${error}`, 'err');
      }
    };
    const warning = statusDeliveryWarning(result.mutation);
    const success = `Changed ${field} → ${value} — Compare again (Ctrl+R)${warning}`;
    try {
      await refreshJobs();
      setStatusAction(success, `Undo ${field} change`, undo, warning ? 'err' : '');
    } catch (error) {
      setStatusAction(`${success} · job-list refresh failed: ${error}`, `Undo ${field} change`, undo, 'err');
    }
  }, [
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
    refreshJobs,
    resetSafetyUi,
    rootSaveInFlightRef,
    rootSaveRequestIdRef,
    setStatus,
    setStatusAction,
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
      setStatus(await describeMutationFailure(
        request.owner.jobName,
        'Root swap failed',
        error,
      ), 'err');
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
    const undo = async () => {
      let restored: JobRootMutationDto;
      try {
        restored = await jobsIpc.swapJobRoots(
          result.mutation.name,
          result.mutation.job_id,
          result.mutation.config_revision,
          request.owner.targetIndex,
        );
      } catch (error) {
        throw new Error(await describeMutationFailure(
          result.mutation.name,
          'Could not undo the root swap',
          error,
        ));
      }
      const restoredState = rootMutationState(restored, request.owner.targetIndex);
      dispatchRootEditor({
        type: 'workspace_rebound',
        workspaceKey: request.workspaceKey,
        owner: restoredState.owner,
        values: restoredState.values,
      });
      reconcileSavedWorkspaceJob(restored.mutation, {
        jobId: result.mutation.job_id,
        name: result.mutation.name,
        configRevision: result.mutation.config_revision,
      });
      resetSafetyUi();
      const warning = statusDeliveryWarning(restored.mutation);
      try {
        await refreshJobs();
        setStatus(`Restored the two roots of '${restored.mutation.name}'${warning}`, warning ? 'err' : '');
      } catch (error) {
        setStatus(`Restored the two roots of '${restored.mutation.name}'${warning} · job-list refresh failed: ${error}`, 'err');
      }
    };
    const warning = statusDeliveryWarning(result.mutation);
    const success = `Swapped target ${request.owner.targetIndex + 1} and source for '${result.mutation.name}' — Compare again (Ctrl+R)${warning}`;
    try {
      await refreshJobs();
      setStatusAction(success, 'Undo swap', undo, warning ? 'err' : '');
    } catch (error) {
      setStatusAction(`${success} · job-list refresh failed: ${error}`, 'Undo swap', undo, 'err');
    }
  }, [
    describeMutationFailure,
    dispatchRootEditor,
    pushHistory,
    reconcileSavedWorkspaceJob,
    refreshJobs,
    resetSafetyUi,
    setBusy,
    setStatus,
    setStatusAction,
  ]);

  return { saveRootDraft, requestSwap, doSwap };
}
