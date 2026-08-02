export type WhenFinishedAction = 'none' | 'sleep' | 'shutdown';

export type PowerAction = Exclude<WhenFinishedAction, 'none'>;

export interface CompletedApplySummary {
  cancelled?: boolean;
  errors?: number;
}

interface PowerActionCountdownFacts {
  readyRunId: number | null;
  currentRunId: number;
  summary: CompletedApplySummary | null;
  applying: boolean;
  autoCloseEnabled: boolean;
  whenFinishedAction: WhenFinishedAction;
}

export interface PowerActionCountdown {
  action: PowerAction;
  runId: number;
  secondsRemaining: number;
}

export function derivePowerActionCountdown(
  facts: PowerActionCountdownFacts,
): PowerActionCountdown | null {
  if (facts.readyRunId !== facts.currentRunId
    || !facts.summary
    || facts.summary.cancelled !== false
    || facts.summary.errors !== 0
    || !facts.applying
    || facts.autoCloseEnabled
    || facts.whenFinishedAction === 'none'
  ) {
    return null;
  }
  return {
    action: facts.whenFinishedAction,
    runId: facts.currentRunId,
    secondsRemaining: 10,
  };
}

interface AutoCloseFacts {
  completedRunId: number;
  currentRunId: number;
  summary: CompletedApplySummary | null;
  applying: boolean;
  autoCloseEnabled: boolean;
  closeAfterStop: boolean;
}

export interface AutoCloseRequest {
  runId: number;
}

export function deriveAutoCloseRequest(facts: AutoCloseFacts): AutoCloseRequest | null {
  if (facts.completedRunId !== facts.currentRunId
    || !facts.summary
    || facts.summary.cancelled !== false
    || facts.summary.errors !== 0
    || !facts.applying
    || !facts.autoCloseEnabled
    || facts.closeAfterStop
  ) {
    return null;
  }
  return { runId: facts.completedRunId };
}
