import { useRef, useState } from 'react';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import { useJobRegistryController } from '#ui/features/jobs/hooks/useJobRegistryController.ts';

export function useWorkspaceSessionState() {
  const registry = useJobRegistryController();
  const [selectedTargetIndex, setSelectedTargetIndex] = useState(0);
  const selectionRef = useRef<{ job: JobDto | null; targetIndex: number }>({ job: null, targetIndex: 0 });
  selectionRef.current = { job: registry.selectedJob, targetIndex: selectedTargetIndex };

  return {
    jobs: registry.jobs,
    selectedJob: registry.selectedJob,
    refreshJobs: registry.refresh,
    setRegistrySelection: registry.select,
    selectedTargetIndex,
    setSelectedTargetIndex,
    selectionRef,
  };
}
