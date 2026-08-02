import { useCallback } from 'react';
import type { Dispatch, MutableRefObject, SetStateAction } from 'react';
import {
  activeWorkspace,
  preferredTargetIndex,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type {
  CompareWorkspace,
  CompareWorkspaceRepository,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

interface WorkspaceSelectionActionsOptions {
  busy: boolean;
  repository: CompareWorkspaceRepository;
  selectedJob: JobDto | null;
  selectionRef: MutableRefObject<{ job: JobDto | null; targetIndex: number }>;
  setRegistrySelection: (job: JobDto | null) => void;
  setSelectedTargetIndex: Dispatch<SetStateAction<number>>;
  resetSafetyUi: () => void;
  requestResultRestore: (
    job: JobDto,
    targetIndex: number,
    retained: CompareWorkspace | null,
    announce?: boolean,
  ) => void;
  setStatus: StatusApi['setMessage'];
}

/** Couples registry/target selection with retained-result restoration and safety resets. */
export function useWorkspaceSelectionActions({
  busy,
  repository,
  selectedJob,
  selectionRef,
  setRegistrySelection,
  setSelectedTargetIndex,
  resetSafetyUi,
  requestResultRestore,
  setStatus,
}: WorkspaceSelectionActionsOptions) {
  const selectJob = useCallback((job: JobDto) => {
    if (selectedJob?.job_id === job.job_id) return;
    const targetIndex = preferredTargetIndex(repository, job);
    const restored = activeWorkspace(repository, job, targetIndex);
    selectionRef.current = { job, targetIndex };
    setRegistrySelection(job);
    setSelectedTargetIndex(targetIndex);
    resetSafetyUi();
    setStatus(restored
      ? `${job.name} · restored ${restored.plan.ops.length} compare items`
      : `${job.name} · ${job.mode}${job.rigor !== 'standard' ? ` · ${job.rigor}` : ''}`);
    requestResultRestore(job, targetIndex, restored);
  }, [
    repository,
    requestResultRestore,
    resetSafetyUi,
    selectedJob?.job_id,
    selectionRef,
    setRegistrySelection,
    setSelectedTargetIndex,
    setStatus,
  ]);

  const selectTarget = useCallback((targetIndex: number) => {
    if (busy || !selectedJob) return;
    const targetPath = selectedJob.targets[targetIndex];
    if (targetIndex === selectionRef.current.targetIndex || targetPath === undefined) return;
    const restored = activeWorkspace(repository, selectedJob, targetIndex);
    selectionRef.current = { job: selectedJob, targetIndex };
    setSelectedTargetIndex(targetIndex);
    resetSafetyUi();
    setStatus(restored
      ? `Switched target → ${targetPath} · restored ${restored.plan.ops.length} compare items`
      : `Switched target → ${targetPath} — Compare again (Ctrl+R)`);
    requestResultRestore(selectedJob, targetIndex, restored);
  }, [
    busy,
    repository,
    requestResultRestore,
    resetSafetyUi,
    selectedJob,
    selectionRef,
    setSelectedTargetIndex,
    setStatus,
  ]);

  return { selectJob, selectTarget };
}
