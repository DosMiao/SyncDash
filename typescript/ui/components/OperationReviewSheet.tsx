import { useId } from 'react';
import { Sheet } from './ui';
import {
  directAuthorization,
  operationReviewCanSubmit,
  type ApprovalChoices,
  type OperationReviewState,
} from '../state/operation-review';
import type { OperationReviewDto } from '../../core/types/generated/OperationReviewDto';

function severityLabel(severity: 'block' | 'needs_ack' | 'info'): string {
  if (severity === 'block') return 'Blocks operation';
  if (severity === 'needs_ack') return 'Needs acceptance';
  return 'Information';
}

export function OperationReviewDetails({
  state,
  choices,
  onChoices,
}: {
  state: OperationReviewState;
  choices: ApprovalChoices;
  onChoices: (choices: ApprovalChoices) => void;
}) {
  const headingPrefix = useId();
  const review = state.review;
  if (state.phase === 'reviewing') {
    return <div className="review-status" role="status" aria-live="polite">Checking current health and capabilities…</div>;
  }
  if (!review) {
    return (
      <div className="review-status danger" role="alert">
        {state.error ? `Safety review failed: ${state.error}` : 'No safety review is available.'}
      </div>
    );
  }

  return (
    <>
      {review.status === 'direct_authorized' && (
        directAuthorization(review)
          ? <div className="review-status ok" role="status">All required checks passed. This exact operation is authorized.</div>
          : <div className="review-status danger" role="alert">The review did not provide the required authorization. Close it and review again.</div>
      )}
      {state.phase === 'approving' && (
        <div className="review-status" role="status" aria-live="polite">Authorizing the exact reviewed operation…</div>
      )}
      {review.expires_at_ms && review.status === 'confirmation_required' && (
        <div className="review-expiry">
          Approval request expires at {new Date(review.expires_at_ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}.
          If it expires, close this sheet and review again.
        </div>
      )}
      {review.blockers.length > 0 && (
        <section className="review-section review-blockers" aria-labelledby={`${headingPrefix}-blockers`}>
          <h4 id={`${headingPrefix}-blockers`}>Blocking conditions</h4>
          <ul>{review.blockers.map((item, index) => <li key={`${index}-${item}`}>{item}</li>)}</ul>
        </section>
      )}
      {review.warnings.length > 0 && (
        <section className="review-section review-warnings" aria-labelledby={`${headingPrefix}-warnings`}>
          <h4 id={`${headingPrefix}-warnings`}>Warnings</h4>
          <ul>{review.warnings.map((item, index) => <li key={`${index}-${item}`}>{item}</li>)}</ul>
        </section>
      )}
      {review.capabilities.length > 0 && (
        <section className="review-section" aria-labelledby={`${headingPrefix}-capabilities`}>
          <h4 id={`${headingPrefix}-capabilities`}>Endpoint capabilities</h4>
          <div className="capability-list">
            {review.capabilities.map((capability, index) => (
              <article className={`capability-card severity-${capability.severity}`} key={`${capability.side}-${capability.feature}-${index}`}>
                <header>
                  <b>{capability.feature}</b>
                  <span>{capability.side}</span>
                  <span className="capability-severity">{severityLabel(capability.severity)}</span>
                </header>
                <dl>
                  <div><dt>Requested</dt><dd>{capability.requested}</dd></div>
                  <div><dt>Available</dt><dd>{capability.actual}</dd></div>
                  <div><dt>Effect</dt><dd>{capability.effect}</dd></div>
                </dl>
              </article>
            ))}
          </div>
        </section>
      )}
      {review.status === 'blocked' && (
        <div className="review-status danger" role="alert">
          Resolve the blocking conditions and run Compare again. This operation cannot be approved.
        </div>
      )}
      {review.status === 'confirmation_required' && (
        review.challenge_id
          ? (
            <ApprovalControls
              review={review}
              choices={choices}
              disabled={state.phase !== 'ready'}
              onChoices={onChoices}
            />
          )
          : <div className="review-status danger" role="alert">The review did not provide an approval challenge. Close it and review again.</div>
      )}
      {state.error && (
        <div className="review-status danger" role="alert">
          Authorization failed: {state.error}. Close this sheet and run the safety review again.
        </div>
      )}
    </>
  );
}

function ApprovalControls({
  review,
  choices,
  disabled,
  onChoices,
}: {
  review: OperationReviewDto;
  choices: ApprovalChoices;
  disabled: boolean;
  onChoices: (choices: ApprovalChoices) => void;
}) {
  const update = (patch: Partial<ApprovalChoices>) => {
    const next = { ...choices, ...patch };
    if (!next.rememberForSession) next.allowUnattended = false;
    onChoices(next);
  };
  return (
    <fieldset className="review-approvals" disabled={disabled}>
      <legend>Approval and session options</legend>
      {review.requires_health_ack && (
        <label className="review-check">
          <input
            type="checkbox"
            checked={choices.acknowledgeHealth}
            onChange={(event) => update({ acknowledgeHealth: event.target.checked })}
          />
          <span><b>I acknowledge the health warnings above.</b> I understand the exact operation may exceed its configured safety thresholds.</span>
        </label>
      )}
      {review.requires_capability_ack && (
        <label className="review-check">
          <input
            type="checkbox"
            checked={choices.acceptCapabilities}
            onChange={(event) => update({ acceptCapabilities: event.target.checked })}
          />
          <span><b>I accept the capability differences above.</b> I understand their stated effects on this operation.</span>
        </label>
      )}
      {review.can_remember_for_session && (
        <label className="review-check review-optional">
          <input
            type="checkbox"
            checked={choices.rememberForSession}
            onChange={(event) => update({ rememberForSession: event.target.checked })}
          />
          <span><b>Remember this job, revision, target, and capability grant for this session.</b> It is not written to the job or persisted across restarts.</span>
        </label>
      )}
      {review.can_allow_unattended && (
        <label className="review-check review-optional">
          <input
            type="checkbox"
            checked={choices.allowUnattended}
            disabled={!choices.rememberForSession}
            onChange={(event) => update({ allowUnattended: event.target.checked })}
          />
          <span><b>Allow unattended apply for this session grant.</b> Requires “Remember”; every unattended run is still health-checked and bound to its exact server-validated action set.</span>
        </label>
      )}
    </fieldset>
  );
}

export function CompareReviewSheet({
  state,
  choices,
  onChoices,
  onCancel,
  onApprove,
}: {
  state: OperationReviewState;
  choices: ApprovalChoices;
  onChoices: (choices: ApprovalChoices) => void;
  onCancel: () => void;
  onApprove: () => void;
}) {
  const review = state.review;
  const blocked = review?.status === 'blocked';
  return (
    <Sheet
      title={blocked ? 'Compare is blocked' : 'Review Compare authorization'}
      width="mid"
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>
            {blocked || state.phase === 'error' ? 'Close' : 'Cancel (Esc)'}
          </button>
          {!blocked && (
            <button
              type="button"
              className="btn accent"
              disabled={!operationReviewCanSubmit(state, choices)}
              onClick={onApprove}
            >
              {state.phase === 'approving' ? 'Authorizing…' : 'Approve & Compare'}
            </button>
          )}
        </>
      }
    >
      <OperationReviewDetails state={state} choices={choices} onChoices={onChoices} />
    </Sheet>
  );
}
