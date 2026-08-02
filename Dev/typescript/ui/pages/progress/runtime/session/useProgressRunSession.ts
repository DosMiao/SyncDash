import { useCallback, useEffect, useRef, useState } from 'react';
import {
  calculateWindowRate,
  newRunState,
} from '#core/application/progress/runstate.ts';
import type { RunState } from '#core/application/progress/runstate.ts';

/** Owns the mutable run snapshot and the deliberately throttled render clock. */
export function useProgressRunSession() {
  const runStateRef = useRef<RunState>(newRunState());
  const [, setRenderRevision] = useState(0);
  const requestRender = useCallback(() => setRenderRevision((revision) => revision + 1), []);

  useEffect(() => {
    const renderIntervalId = window.setInterval(() => {
      if (runStateRef.current.runId >= 0) requestRender();
    }, 500);
    return () => window.clearInterval(renderIntervalId);
  }, [requestRender]);

  const formatByteRate = useCallback((runState: RunState) => {
    const rate = calculateWindowRate(runState, 4000);
    return rate ? `${(rate.bytesPerSecond / (1 << 20)).toFixed(2)} MiB/s` : '';
  }, []);
  const formatItemRate = useCallback((runState: RunState) => {
    const rate = calculateWindowRate(runState, 4000);
    return rate ? `${rate.itemsPerSecond.toFixed(0)} items/s` : '';
  }, []);

  return {
    currentRunState: runStateRef.current,
    formatByteRate,
    formatItemRate,
    requestRender,
    runStateRef,
  };
}
