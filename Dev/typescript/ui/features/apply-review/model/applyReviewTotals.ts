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
