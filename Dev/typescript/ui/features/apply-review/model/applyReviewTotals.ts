import { effectiveOperation, rowTransferBytes, type PlanDto } from '#core/domain/compare/plan.ts';

export interface ApplyReviewTotals {
  copyCount: number;
  updateCount: number;
  moveCount: number;
  deleteCount: number;
  transferBytes: number;
  deletionBytes: number;
  reversedCount: number;
  checkedOutsideScope: number;
  /** The target's own entry count, which every share below is measured against. */
  targetEntries: number;
}

export function summarizeApplyReview(
  plan: PlanDto,
  executableIndices: number[],
  reversedRows: boolean[],
  includedRows: boolean[],
): ApplyReviewTotals {
  const totals: ApplyReviewTotals = {
    copyCount: 0,
    updateCount: 0,
    moveCount: 0,
    deleteCount: 0,
    transferBytes: 0,
    deletionBytes: 0,
    reversedCount: 0,
    checkedOutsideScope: 0,
    targetEntries: plan.header.target_entries,
  };
  for (const index of executableIndices) {
    const operation = effectiveOperation(plan, reversedRows, index);
    if (operation.action === 'copy') {
      totals.copyCount++;
      totals.transferBytes += rowTransferBytes(plan, reversedRows, index);
    } else if (operation.action === 'update') {
      totals.updateCount++;
      totals.transferBytes += rowTransferBytes(plan, reversedRows, index);
    } else if (operation.action === 'chmod') {
      totals.updateCount++;
    } else if (operation.action === 'move') {
      totals.moveCount++;
    } else if (operation.action === 'delete' || operation.action === 'delete_dir') {
      totals.deleteCount++;
      totals.deletionBytes += operation.size ?? 0;
    }
    if (reversedRows[index]) totals.reversedCount++;
  }
  totals.checkedOutsideScope = includedRows.filter(Boolean).length - executableIndices.length;
  return totals;
}

/**
 * How much of what the target already holds a category of the run displaces.
 *
 * Measured against the target's entry count rather than the plan's own size: "1000 of 1000 rows are
 * deletes" says nothing on its own, while "1000 of 1200 entries" is the number that distinguishes a
 * deliberate cleanup from a wrong filter, a swapped source and target, or an unmounted share.
 *
 * Only pass a count of operations that displace existing data. A plain copy publishes onto a name
 * the target does not have, so it is not part of any share — counting copies here reported
 * "792350% of target" for a first mirror onto a near-empty disk, which is arithmetic rather than
 * information. Returns null when there is nothing to measure against, so a caller shows no share at
 * all rather than a made-up zero.
 */
export function planShare(count: number, targetEntries: number): number | null {
  if (count <= 0 || targetEntries <= 0) return null;
  return count / targetEntries;
}

/**
 * Whether a category is large enough that the review panel should color it.
 *
 * A threshold outside (0, 1) is the job switching the highlight off, matching how the engine reads
 * the same number. This marks a row for attention; it never withholds the run.
 */
export function planShareIsHigh(
  count: number,
  targetEntries: number,
  threshold: number,
): boolean {
  if (!(threshold > 0 && threshold < 1)) return false;
  const share = planShare(count, targetEntries);
  return share !== null && share >= threshold;
}

export function formatPlanShare(count: number, targetEntries: number): string {
  const share = planShare(count, targetEntries);
  return share === null ? '' : `${Math.round(share * 100)}% of target`;
}
