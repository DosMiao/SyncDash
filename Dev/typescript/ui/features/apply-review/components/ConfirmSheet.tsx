import { humanSize } from '#core/shared/format.ts';
import {
  operationReviewCanSubmit,
  operationReviewExpired,
  operationReviewFailed,
  type OperationReviewState,
} from '#core/application/operations/operationReview.ts';
import { OperationReviewDetails } from '#ui/features/operation-review/components/OperationReviewSheet.tsx';
import { Sheet } from '#ui/shared/components/overlays/Sheet.tsx';
import type { JobDto } from '#core/types/generated/JobDto.ts';

import {
  formatPlanShare,
  planShareIsHigh,
  type ApplyReviewTotals,
} from '#ui/features/apply-review/model/applyReviewTotals.ts';

interface ConfirmSheetProps {
  job: JobDto;
  totals: ApplyReviewTotals;
  reviewState: OperationReviewState;
  onCancel: () => void;
  onConfirm: () => void;
  onReviewAgain: () => void;
}

export function ConfirmSheet(props: ConfirmSheetProps) {
  const { job, totals, reviewState, onCancel, onConfirm, onReviewAgain } = props;
  const blocked = reviewState.review?.status === 'blocked';
  // The same share the engine reports and the job's Gates chip advertises. It colors the row and
  // names the number; it never decides whether Apply may run.
  //
  // The Copy/Update row keys on updates alone: an update overwrites an entry the target already
  // has, while a copy adds a name it does not, so only the update half can displace anything.
  const updateIsHigh = planShareIsHigh(totals.updateCount, totals.targetEntries, job.max_delete_ratio);
  const moveIsHigh = planShareIsHigh(totals.moveCount, totals.targetEntries, job.max_delete_ratio);
  const deleteIsHigh = planShareIsHigh(
    totals.deleteCount,
    totals.targetEntries,
    job.max_delete_ratio,
  );
  const canApply = operationReviewCanSubmit(reviewState);
  const actionLabel = reviewState.phase === 'reviewing'
    ? 'Reviewing…'
    : reviewState.phase === 'approving'
      ? 'Authorizing…'
      : operationReviewExpired(reviewState)
        ? 'Review Again'
        : operationReviewFailed(reviewState)
        ? 'Review failed'
        : 'Apply';

  return (
    <Sheet
      title="Review & Apply"
      width="mid"
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>
            {blocked || operationReviewFailed(reviewState) ? 'Close' : 'Cancel (Esc)'}
          </button>
          <button
            type="button"
            className="btn accent"
            disabled={!canApply && !operationReviewExpired(reviewState)}
            onClick={operationReviewExpired(reviewState) ? onReviewAgain : onConfirm}
          >
            {actionLabel}
          </button>
        </>
      }
    >
      <div className="mrow">
        <span>Job</span><b>{job.name}</b><span className={'mode ' + job.mode}>{job.mode}</span>
      </div>
      <div className={'mrow' + (updateIsHigh ? ' danger' : '')}>
        <span>Copy / Update</span><b>{totals.copyCount} / {totals.updateCount}</b>
        <span className="dim">{humanSize(totals.transferBytes) || '0 B'}</span>
        {updateIsHigh && (
          <span className="dim">
            {formatPlanShare(totals.updateCount, totals.targetEntries)} overwritten
          </span>
        )}
      </div>
      <div className={'mrow' + (moveIsHigh ? ' danger' : '')}>
        <span>Move (No Re-transfer)</span><b>{totals.moveCount}</b>
        {moveIsHigh && (
          <span className="dim">{formatPlanShare(totals.moveCount, totals.targetEntries)} moved</span>
        )}
      </div>
      <div className={'mrow' + (totals.deleteCount ? ' danger' : '')}>
        <span>Delete (Into the Trash)</span><b>{totals.deleteCount}</b>
        <span className="dim">{totals.deleteCount ? humanSize(totals.deletionBytes) : ''}</span>
        {deleteIsHigh && (
          <span className="dim">
            {formatPlanShare(totals.deleteCount, totals.targetEntries)} deleted
          </span>
        )}
      </div>
      {totals.reversedCount > 0 && (
        <div className="mrow warn"><span>Of Those, Reversed</span><b>{totals.reversedCount}</b></div>
      )}
      {totals.checkedOutsideScope > 0 && (
        <div className="mrow warn">
          <span>Included but Outside Run Scope</span><b>{totals.checkedOutsideScope}</b>
          <span className="dim">Only included differences in scope are applied</span>
        </div>
      )}
      <OperationReviewDetails state={reviewState} />
    </Sheet>
  );
}
