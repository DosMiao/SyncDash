import { useId } from 'react';
import { Sheet } from '#ui/shared/components/overlays/Sheet.tsx';
import {
  directAuthorization,
  isConfirmationReview,
  operationReviewCanSubmit,
  operationReviewExpired,
  operationReviewFailed,
  type ApprovalChoices,
  type ConfirmationReview,
  type OperationReviewState,
} from '#core/application/operations/operationReview.ts';

function severityLabel(severity: 'unavailable' | 'degraded' | 'info'): string {
  if (severity === 'unavailable') return 'Not available here';
  if (severity === 'degraded') return 'Works differently here';
  return 'Information';
}

export function OperationReviewDetails({ state }: { state: OperationReviewState }) {
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
  const blockers = review.status === 'blocked' ? review.blockers : [];
  const warnings = review.status === 'blocked'
    || review.status === 'interactive_apply_confirmation_required'
    ? review.warnings
    : [];

  return (
    <>
      {state.phase === 'expired' && (
        <div className="review-status danger" role="alert">
          {state.error}
        </div>
      )}
      {review.status === 'direct_authorized' && (
        directAuthorization(review)
          ? <div className="review-status ok" role="status">All required checks passed. This exact operation is authorized.</div>
          : <div className="review-status danger" role="alert">The review did not provide the required authorization. Close it and review again.</div>
      )}
      {state.phase === 'approving' && (
        <div className="review-status" role="status" aria-live="polite">Authorizing the exact reviewed operation…</div>
      )}
      {isConfirmationReview(review) && (
        <div className="review-expiry">
          Approval request expires at {new Date(review.expires_at_ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}.
          The controls disable automatically before that deadline.
        </div>
      )}
      {blockers.length > 0 && (
        <section className="review-section review-blockers" aria-labelledby={`${headingPrefix}-blockers`}>
          <h4 id={`${headingPrefix}-blockers`}>Blocking conditions</h4>
          <ul>{blockers.map((item, index) => <li key={`${index}-${item}`}>{item}</li>)}</ul>
        </section>
      )}
      {warnings.length > 0 && (
        <section className="review-section review-warnings" aria-labelledby={`${headingPrefix}-warnings`}>
          <h4 id={`${headingPrefix}-warnings`}>Warnings</h4>
          <ul>{warnings.map((item, index) => <li key={`${index}-${item}`}>{item}</li>)}</ul>
        </section>
      )}
      {review.capabilities.length > 0 && (
        <section className="review-section" aria-labelledby={`${headingPrefix}-capabilities`}>
          <h4 id={`${headingPrefix}-capabilities`}>What this run will do differently</h4>
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
      {state.error && state.phase !== 'expired' && (
        <div className="review-status danger" role="alert">
          Authorization failed: {state.error}. Close this sheet and run the safety review again.
        </div>
      )}
    </>
  );
}

export function CompareReviewSheet({
  state,
  choices,
  onChoices,
  onCancel,
  onApprove,
  onReviewAgain,
}: {
  state: OperationReviewState;
  choices: ApprovalChoices;
  onChoices: (choices: ApprovalChoices) => void;
  onCancel: () => void;
  onApprove: () => void;
  onReviewAgain: () => void;
}) {
  const review = state.review;
  const blocked = review?.status === 'blocked';
  return (
    <Sheet
      title={blocked
        ? 'Compare is blocked'
        : operationReviewFailed(state)
          ? 'Compare review failed'
          : 'Review Compare authorization'}
      width="mid"
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>
            {blocked || operationReviewFailed(state) ? 'Close' : 'Cancel (Esc)'}
          </button>
          {operationReviewExpired(state) ? (
            <button type="button" className="btn accent" onClick={onReviewAgain}>
              Review Again
            </button>
          ) : !blocked && !operationReviewFailed(state) && (
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
      <OperationReviewDetails state={state} />
    </Sheet>
  );
}
