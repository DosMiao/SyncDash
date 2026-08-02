import { useCallback, useRef, useState } from 'react';
import { cancelCompareRun } from '#core/infrastructure/tauri/commands/compare.ts';
import type { CompareStage } from '#core/domain/compare/compareProgress.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

/** Mutable state and fences that belong to one compare execution stream. */
export function useCompareRunState(setStatus: StatusApi['setMessage']) {
  const [active, setActive] = useState(false);
  const [stages, setStages] = useState<CompareStage[]>([]);
  const [cancelling, setCancelling] = useState(false);
  // The 0.7/0.3 EMA prevents per-file size swings from dominating the compare rate.
  const rateByPhase = useRef(new Map<string, {
    timestampMs: number;
    bytesDone: number;
    smoothedRate: number;
  }>());
  const runId = useRef(-1);
  const runFloor = useRef(-1);
  const runReady = useRef(false);
  const inFlight = useRef(false);
  const activityRequestId = useRef(0);
  const restoreRequestId = useRef(0);

  const cancel = useCallback(() => {
    if (!runReady.current || runId.current < 0) {
      setStatus('Compare is still starting — cancel will be available when its run is registered');
      return;
    }
    const cancellingRunId = runId.current;
    setCancelling(true);
    setStatus('Cancelling the compare…');
    void cancelCompareRun(cancellingRunId).then((accepted) => {
      if (accepted) return;
      setCancelling(false);
      setStatus('That compare already finished; no newer run was cancelled');
    }).catch((error) => {
      setCancelling(false);
      setStatus(`Cancel failed: ${error}`, 'err');
    });
  }, [setStatus]);

  return {
    active,
    activityRequestId,
    cancel,
    cancelling,
    inFlight,
    rateByPhase,
    restoreRequestId,
    runFloor,
    runId,
    runReady,
    setActive,
    setCancelling,
    setStages,
    stages,
  };
}
