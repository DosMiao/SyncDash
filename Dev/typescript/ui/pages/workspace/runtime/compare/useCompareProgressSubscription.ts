import { eventPhase } from '#core/domain/runs/runEvents.ts';
import { useEffect, type Dispatch, type MutableRefObject, type SetStateAction } from 'react';
import { listenToMainWindowEvent } from '#core/infrastructure/tauri/mainWindow.ts';
import { replayCompareEvents } from '#core/infrastructure/tauri/commands/compare.ts';
import { mergeRunEventReplay } from '#core/domain/runs/runEvents.ts';
import {
  NO_COMPARE_RUN_FAULTS,
  recordCompareFault,
  reduceCompareStages,
} from '#core/domain/compare/compareProgress.ts';
import type {
  CompareProgressEvent,
  CompareRunFaults,
  CompareStage,
} from '#core/domain/compare/compareProgress.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

export interface CompareRateSample {
  timestampMs: number;
  bytesDone: number;
  smoothedRate: number;
}

interface CompareProgressSubscription {
  runReady: MutableRefObject<boolean>;
  runFloor: MutableRefObject<number>;
  runId: MutableRefObject<number>;
  rateByPhase: MutableRefObject<Map<string, CompareRateSample>>;
  setFaults: Dispatch<SetStateAction<CompareRunFaults>>;
  setStages: Dispatch<SetStateAction<CompareStage[]>>;
  setStatus: StatusApi['setMessage'];
}

/// Reconciles replayed and live Compare events before publishing progress into the page. The refs
/// are page-owned because launching and cancelling Compare share the same run-identity fence.
export function useCompareProgressSubscription({
  runReady,
  runFloor,
  runId,
  rateByPhase,
  setFaults,
  setStages,
  setStatus,
}: CompareProgressSubscription) {
  useEffect(() => {
    let disposed = false;
    let dispose: (() => void) | undefined;
    let ready = false;
    let lastSequence = 0;
    const queued: CompareProgressEvent[] = [];
    const handle = (event: CompareProgressEvent) => {
      if (event.purpose !== 'compare') return;
      if (!runReady.current && event.run_id <= runFloor.current) return;
      if (event.run_id < runId.current) return;
      if (event.run_id > runId.current) {
        runId.current = event.run_id;
        rateByPhase.current.clear();
        setStages([]);
        // A new run's faults are its own. Carrying the previous run's over would report a file the
        // user may already have fixed.
        setFaults(NO_COMPARE_RUN_FAULTS);
      }
      runReady.current = true;
      // Error events are actionable even when the producer cannot associate them with a phase.
      if (event.kind === 'error') {
        setStatus(`${event.action === 'walk' ? 'Scan could not read' : 'Error'}: ${event.message ?? ''}`, 'err');
        // The status line is one slot the next message overwrites; the mark keeps it.
        setFaults((previous) => recordCompareFault(previous, event));
        return;
      }
      if (!eventPhase(event)) return;
      if (event.kind === 'phase_start') {
        setStages((previous) => reduceCompareStages(previous, event));
      } else if (event.kind === 'totals') {
        const timestampMs = event.ts_ms ?? Date.now();
        const bytesDone = event.bytes_done ?? 0;
        rateByPhase.current.set(event.phase, { timestampMs, bytesDone, smoothedRate: 0 });
        setStages((previous) => reduceCompareStages(previous, event));
      } else if (event.kind === 'progress') {
        const timestampMs = event.ts_ms ?? Date.now();
        const bytesDone = event.bytes_done ?? 0;
        const previousRate = rateByPhase.current.get(event.phase);
        let smoothedRate = 0;
        if (previousRate
          && timestampMs > previousRate.timestampMs
          && bytesDone >= previousRate.bytesDone) {
          const instantaneousRate = ((bytesDone - previousRate.bytesDone) * 1000)
            / (timestampMs - previousRate.timestampMs);
          smoothedRate = previousRate.smoothedRate > 0
            ? previousRate.smoothedRate * 0.7 + instantaneousRate * 0.3
            : instantaneousRate;
          rateByPhase.current.set(event.phase, { timestampMs, bytesDone, smoothedRate });
        } else if (!previousRate) {
          rateByPhase.current.set(event.phase, { timestampMs, bytesDone, smoothedRate: 0 });
        } else {
          smoothedRate = previousRate.smoothedRate;
        }
        setStages((previous) => reduceCompareStages(previous, event, smoothedRate));
      } else if (event.kind === 'phase_end') {
        setStages((previous) => reduceCompareStages(previous, event));
      }
    };
    const publish = (event: CompareProgressEvent) => {
      if (!ready) {
        queued.push(event);
        return;
      }
      if (event.sequence <= lastSequence) return;
      lastSequence = event.sequence;
      handle(event);
    };
    void listenToMainWindowEvent<CompareProgressEvent>('run-progress', (event) => publish(event.payload))
      .then(async (unlisten) => {
        if (disposed) {
          unlisten();
          return;
        }
        dispose = unlisten;
        let replay: CompareProgressEvent[] = [];
        try {
          replay = await replayCompareEvents();
        } catch (error) {
          setStatus(`Could not restore compare progress after reconnect: ${error}`, 'err');
        }
        if (disposed) return;
        const pending = mergeRunEventReplay(replay, queued);
        queued.length = 0;
        ready = true;
        for (const event of pending) publish(event);
      })
      .catch((error) => {
        if (!disposed) setStatus(`Could not subscribe to compare progress: ${error}`, 'err');
      });
    return () => {
      disposed = true;
      dispose?.();
    };
  }, [rateByPhase, runFloor, runId, runReady, setFaults, setStages, setStatus]);
}
