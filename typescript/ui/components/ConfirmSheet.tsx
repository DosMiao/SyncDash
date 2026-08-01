import { humanSize } from '../../core/format';
import {
  operationReviewCanSubmit,
  operationReviewFailed,
  type ApprovalChoices,
  type OperationReviewState,
} from '../state/operationReview';
import { OperationReviewDetails } from './OperationReviewSheet';
import { Sheet } from './ui';
import type { JobDto } from '../../core/types/generated/JobDto';

export interface ApplyReviewTotals {
  copyCount: number;
  updateCount: number;
  moveCount: number;
  deleteCount: number;
  transferBytes: number;
  deletionBytes: number;
  reversedCount: number;
  checkedOutsideScope: number;
}

interface ConfirmSheetProps {
  job: JobDto;
  totals: ApplyReviewTotals;
  reviewState: OperationReviewState;
  choices: ApprovalChoices;
  onChoices: (choices: ApprovalChoices) => void;
  onCancel: () => void;
  onConfirm: () => void;
}

export function ConfirmSheet(props: ConfirmSheetProps) {
  const { job, totals, reviewState, choices, onChoices, onCancel, onConfirm } = props;
  const blocked = reviewState.review?.status === 'blocked';
  const canApply = operationReviewCanSubmit(reviewState, choices);
  const actionLabel = reviewState.phase === 'reviewing'
    ? 'Reviewing…'
    : reviewState.phase === 'approving'
      ? 'Authorizing…'
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
          <button type="button" className="btn accent" disabled={!canApply} onClick={onConfirm}>
            {actionLabel}
          </button>
        </>
      }
    >
      <div className="mrow">
        <span>Job</span><b>{job.name}</b><span className={'mode ' + job.mode}>{job.mode}</span>
      </div>
      <div className="mrow">
        <span>Copy / Update</span><b>{totals.copyCount} / {totals.updateCount}</b>
        <span className="dim">{humanSize(totals.transferBytes) || '0 B'}</span>
      </div>
      <div className="mrow"><span>Move (No Re-transfer)</span><b>{totals.moveCount}</b></div>
      <div className={'mrow' + (totals.deleteCount ? ' danger' : '')}>
        <span>Delete (Into the Trash)</span><b>{totals.deleteCount}</b>
        <span className="dim">{totals.deleteCount ? humanSize(totals.deletionBytes) : ''}</span>
      </div>
      {totals.reversedCount > 0 && (
        <div className="mrow warn"><span>Of Those, Reversed</span><b>{totals.reversedCount}</b></div>
      )}
      {totals.checkedOutsideScope > 0 && (
        <div className="mrow warn">
          <span>Checked but Outside Run Scope</span><b>{totals.checkedOutsideScope}</b>
          <span className="dim">Only checked rows in scope are applied</span>
        </div>
      )}
      <OperationReviewDetails state={reviewState} choices={choices} onChoices={onChoices} />
    </Sheet>
  );
}
