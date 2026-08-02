import type {
  CompareScopeActivity,
  CompareWorkspace,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type {
  WorkspaceExecutionAccess,
  WorkspaceViewOnlyReason,
} from '#core/application/compare-workspace/compareWorkspaceExecution.ts';

export interface ApplyAvailability {
  available: boolean;
  blockedMessage: string | null;
}

const EXECUTION_BLOCK_MESSAGE: Record<WorkspaceViewOnlyReason, string> = {
  retention_checking: 'This result is being confirmed against the retained backend evidence',
  retention_check_failed: 'The retained backend evidence could not be confirmed; retry Compare before applying',
  retention_missing: 'This result is no longer retained by the backend; Compare again before applying',
  execution_unavailable: 'This result has no current execution authority; Compare again before applying',
  awaiting_compare: 'A newer Compare attempt owns this job and target; wait for it to finish or Compare again',
  compare_in_progress: 'A newer Compare is still running for this job and target',
  compare_failed: 'The newest Compare failed, so this retained result is view-only',
  compare_cancelled: 'The newest Compare was cancelled, so this retained result is view-only',
  superseded: 'A newer exact Compare result superseded this review workspace',
  execution_expired: 'Filesystem writes or job changes expired this result’s execution authority',
};

export function deriveApplyAvailability(input: {
  workspace: CompareWorkspace | null;
  workspaceExecutionAccess: WorkspaceExecutionAccess | null;
  compareActivity: CompareScopeActivity | null;
  scopeCalculationPending: boolean;
  scopeCalculationFailed: boolean;
  executableCount: number;
}): ApplyAvailability {
  if (!input.workspace) {
    return {
      available: false,
      blockedMessage: 'Compare first to create a result that can be reviewed and applied',
    };
  }
  if (!input.workspaceExecutionAccess || input.workspaceExecutionAccess.status === 'view_only') {
    return {
      available: false,
      blockedMessage: input.workspaceExecutionAccess
        ? EXECUTION_BLOCK_MESSAGE[input.workspaceExecutionAccess.reason]
        : 'This result has no current execution authority; Compare again before applying',
    };
  }
  if (input.compareActivity?.status !== undefined && input.compareActivity.status !== 'idle') {
    const blockedMessage = input.compareActivity.status === 'failed'
      ? 'The newest Compare attempt failed; run Compare successfully before applying'
      : 'A newer Compare attempt is still active for this job and target';
    return { available: false, blockedMessage };
  }
  if (input.workspace.selectedView !== 'differences') {
    return {
      available: false,
      blockedMessage: 'Switch to Differences to review the run scope before applying it',
    };
  }
  if (input.scopeCalculationFailed) {
    return {
      available: false,
      blockedMessage: 'The run scope could not be calculated safely; clear or revise the failed filter',
    };
  }
  if (input.scopeCalculationPending) {
    return {
      available: false,
      blockedMessage: 'The run scope is still being calculated; wait a moment and try again',
    };
  }
  if (input.executableCount === 0) {
    return {
      available: false,
      blockedMessage: 'No included differences are currently in the run scope',
    };
  }
  return { available: true, blockedMessage: null };
}
