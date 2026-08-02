import type { CompareScopeExecutionStatusDto } from '#core/types/generated/CompareScopeExecutionStatusDto.ts';
import type {
  CompareScopeActivity,
  StagedCompareCandidate,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type {
  WorkspaceExecutionAccess,
  WorkspaceViewOnlyReason,
} from '#core/application/compare-workspace/compareWorkspaceExecution.ts';

export function CompareActivityNotice(props: {
  activity: CompareScopeActivity;
  workspaceExecutionAccess: WorkspaceExecutionAccess | null;
}) {
  if (props.activity.status === 'idle' || props.workspaceExecutionAccess?.status !== 'executable') return null;
  return (
    <section className="compare-workspace-notice view-only" role="status">
      <strong>{props.activity.status === 'failed' ? 'Compare Attempt Failed' : 'New Compare in Progress'}</strong>
      <span>
        {props.activity.status === 'failed'
          ? 'The previous result remains available for inspection, but a successful exact Compare is required before Apply.'
          : 'The previous result remains available for inspection while execution authority moves to the new attempt.'}
      </span>
    </section>
  );
}

function executionExpiryMessage(execution: CompareScopeExecutionStatusDto | null): string {
  if (execution?.status !== 'expired') return 'This result is no longer eligible for Apply.';
  switch (execution.reason) {
    case 'application_restarted':
      return 'The application restarted after this result was created; review it here, then Compare again before Apply.';
    case 'job_changed':
      return 'The job configuration changed after this result was created.';
    case 'job_deleted':
      return 'The job was deleted after this result was created.';
    case 'write_started':
      return 'A write run started from this evidence; Compare again before applying more changes.';
    case 'verification_exhausted':
      return 'The verification sequence was exhausted and execution authority was closed.';
  }
}

function viewOnlyMessage(
  reason: WorkspaceViewOnlyReason,
  execution: CompareScopeExecutionStatusDto | null,
): string {
  switch (reason) {
    case 'retention_checking':
      return 'Checking retained evidence; Apply remains disabled until the exact result is confirmed.';
    case 'retention_check_failed':
      return 'Retention could not be confirmed. The result remains available for inspection only.';
    case 'retention_missing':
      return 'The backend no longer has this exact evidence. The visible snapshot is inspection-only.';
    case 'execution_unavailable':
      return 'Execution authority is unavailable for this result. Run Compare to refresh it.';
    case 'awaiting_compare':
      return 'A newer verification is waiting to run; this result remains visible but cannot be applied.';
    case 'compare_in_progress':
      return 'A newer Compare is in progress; this result remains visible but cannot be applied.';
    case 'compare_failed':
      return execution?.status === 'failed'
        ? `The newer Compare failed: ${execution.message}`
        : 'The newer Compare failed; run it again before applying.';
    case 'compare_cancelled':
      return 'The newer Compare was cancelled; run it again before applying.';
    case 'superseded':
      return 'A newer successful result owns execution authority. Review or adopt that exact result before applying.';
    case 'execution_expired':
      return executionExpiryMessage(execution);
  }
}

export function CompareExecutionNotice(props: {
  access: WorkspaceExecutionAccess;
  execution: CompareScopeExecutionStatusDto | null;
}) {
  if (props.access.status === 'executable') return null;
  return (
    <section className="compare-workspace-notice view-only" role="status">
      <strong>View-Only Result</strong>
      <span>{viewOnlyMessage(props.access.reason, props.execution)}</span>
    </section>
  );
}

export function CompareCandidateNotice(props: {
  candidate: StagedCompareCandidate;
  activeHasReviewEdits: boolean;
  onAdopt: () => void;
  onDiscard: () => void;
}) {
  const plan = props.candidate.workspace.plan;
  return (
    <section className="compare-workspace-notice candidate" role="status">
      <span>
        <strong>Newer AutoScan Result Ready</strong>
        {' · '}{plan.ops.length.toLocaleString()} differences · {plan.header.conflict_count.toLocaleString()} conflicts
        {props.activeHasReviewEdits ? ' · your current review edits are retained until you choose' : ''}
      </span>
      <span className="compare-workspace-notice-actions">
        <button type="button" className="btn accent" onClick={props.onAdopt}>Review New Result</button>
        <button type="button" className="btn" onClick={props.onDiscard}>Dismiss New Result</button>
      </span>
    </section>
  );
}
