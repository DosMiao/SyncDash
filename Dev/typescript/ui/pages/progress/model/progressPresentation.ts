import { isCountedEvent } from '#core/domain/runs/runEvents.ts';
import { humanDuration, humanSize } from '#core/shared/format.ts';
import {
  calculateWindowRate,
  completionPercent,
  type RunProgressEvent,
  type RunState,
} from '#core/application/progress/runstate.ts';
import type { PowerAction } from '#core/application/progress/postRunActions.ts';

export interface PowerActionFailure {
  action: PowerAction;
  runId: number;
  error: string;
}

export interface ProgressPresentation {
  completionPercentage: number;
  roundedCompletionPercentage: number;
  runPaused: boolean;
  finalizing: boolean;
  estimatedTimeRemaining: string;
  progressValueText: string;
  summaryStatusText: string | null;
  errorCount: number;
  warningCount: number;
}

export function formatStageProgress(event: RunProgressEvent): string {
  // Only the counter-bearing variants have totals; anything else formats as no progress.
  const counted = isCountedEvent(event) ? event : null;
  const itemTotal = counted?.items_total ?? 0;
  const byteTotal = counted?.bytes_total ?? 0;
  const completedItems = counted && 'items_done' in counted ? counted.items_done : 0;
  const completedBytes = humanSize(counted && 'bytes_done' in counted ? counted.bytes_done : 0);
  const itemProgress = itemTotal ? `${completedItems} / ${itemTotal}` : `${completedItems}`;
  const byteProgress = byteTotal ? `${completedBytes} / ${humanSize(byteTotal)}` : completedBytes;
  return `${itemProgress} items · ${byteProgress}`;
}

function formatCompletedRunStatus(summary: NonNullable<RunState['summary']>): string {
  if (summary.cancelled) {
    return `Cancelled — ${summary.done} applied, ${summary.skipped} skipped`;
  }
  return [
    `Done — ${summary.done} applied, ${summary.skipped} skipped, ${summary.errors} errors`,
    humanSize(summary.bytes_done ?? 0),
    humanDuration(summary.elapsed_ms ?? 0),
  ].join(' · ');
}

export function deriveProgressPresentation(
  runState: RunState,
  runRejectionMessage: string | null,
): ProgressPresentation {
  const completionPercentage = completionPercent(runState);
  const roundedCompletionPercentage = Math.round(completionPercentage);
  const runPaused = runState.pausedSince > 0;
  const errorCount = runState.errors.filter((entry) => !entry.warning).length;
  const warningCount = runState.errors.length - errorCount;
  const oneMinuteRate = calculateWindowRate(runState, 60000);

  const countersExhausted = runState.totals.items + runState.totals.bytes > 0
    && (runState.totals.items === 0 || runState.completed.items >= runState.totals.items)
    && (runState.totals.bytes === 0 || runState.completed.bytes >= runState.totals.bytes);
  // The copy loop reports its final byte before seal/fsync/verify/preserve/commit completes the
  // item. On an external mirror, preservation may itself be a long cross-volume copy.
  const finalizing = countersExhausted
    || (runState.totals.bytes > 0 && runState.completed.bytes >= runState.totals.bytes);

  let estimatedTimeRemaining: string;
  if (runState.summary) {
    estimatedTimeRemaining = '—';
  } else if (finalizing) {
    estimatedTimeRemaining = 'Finalizing…';
  } else if (oneMinuteRate && runState.totals.bytes > 0 && oneMinuteRate.bytesPerSecond > 1) {
    estimatedTimeRemaining = `${humanDuration(
      Math.max(0, runState.totals.bytes - runState.completed.bytes)
        / oneMinuteRate.bytesPerSecond * 1000,
    )} remaining`;
  } else if (oneMinuteRate && runState.totals.items > 0 && oneMinuteRate.itemsPerSecond > 0.01) {
    estimatedTimeRemaining = `${humanDuration(
      Math.max(0, runState.totals.items - runState.completed.items)
        / oneMinuteRate.itemsPerSecond * 1000,
    )} remaining`;
  } else {
    estimatedTimeRemaining = 'Estimating…';
  }

  const progressValueText = runState.summary
    ? (runState.summary.cancelled
      ? `Stopped at ${roundedCompletionPercentage}%`
      : `${roundedCompletionPercentage}% complete`)
    : runRejectionMessage
      ? 'Run failed to start'
      : runState.running
        ? (runState.applying ? `${roundedCompletionPercentage}% complete` : 'Preparing Apply')
        : 'Waiting for a run';

  return {
    completionPercentage,
    roundedCompletionPercentage,
    runPaused,
    finalizing,
    estimatedTimeRemaining,
    progressValueText,
    summaryStatusText: runState.summary ? formatCompletedRunStatus(runState.summary) : null,
    errorCount,
    warningCount,
  };
}
