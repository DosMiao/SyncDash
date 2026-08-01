import { humanSize } from '../../core/format';
import {
  operationReviewCanSubmit,
  type ApprovalChoices,
  type OperationReviewState,
} from '../state/operation-review';
import { OperationReviewDetails } from './OperationReviewSheet';
import { Sheet } from './ui';
import type { JobDto } from '../../core/types/generated/JobDto';

export interface ConfirmTotals {
  copy: number;
  update: number;
  move: number;
  del: number;
  bytes: number;
  delBytes: number;
  flips: number;
  /// Checked rows a filter is hiding — they will NOT run, and that has to be said out loud
  hiddenChecked: number;
}

interface Props {
  job: JobDto;
  totals: ConfirmTotals;
  reviewState: OperationReviewState;
  choices: ApprovalChoices;
  onChoices: (choices: ApprovalChoices) => void;
  onCancel: () => void;
  onConfirm: () => void;
}

export function ConfirmSheet(props: Props) {
  const { job, totals: t, reviewState, choices, onChoices, onCancel, onConfirm } = props;
  const blocked = reviewState.review?.status === 'blocked';
  const canApply = operationReviewCanSubmit(reviewState, choices);
  const actionLabel = reviewState.phase === 'reviewing'
    ? 'Reviewing…'
    : reviewState.phase === 'approving'
      ? 'Authorizing…'
      : reviewState.phase === 'error'
        ? 'Review failed'
        : 'Apply';

  return (
    <Sheet
      title="Review & apply"
      width="mid"
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>
            {blocked || reviewState.phase === 'error' ? 'Close' : 'Cancel (Esc)'}
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
        <span>Copy / update</span><b>{t.copy} / {t.update}</b>
        <span className="dim">{humanSize(t.bytes) || '0 B'}</span>
      </div>
      <div className="mrow"><span>Move (no re-transfer)</span><b>{t.move}</b></div>
      <div className={'mrow' + (t.del ? ' danger' : '')}>
        <span>Delete (into the trash)</span><b>{t.del}</b>
        <span className="dim">{t.del ? humanSize(t.delBytes) : ''}</span>
      </div>
      {t.flips > 0 && (
        <div className="mrow warn"><span>Of those, reversed</span><b>{t.flips}</b></div>
      )}
      {t.hiddenChecked > 0 && (
        <div className="mrow warn">
          <span>Hidden by filter, not run</span><b>{t.hiddenChecked}</b>
          <span className="dim">The view is the action set</span>
        </div>
      )}
      <OperationReviewDetails state={reviewState} choices={choices} onChoices={onChoices} />
    </Sheet>
  );
}
