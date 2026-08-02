import { effectiveOperation, rowTransferBytes, type PlanDto } from '#core/domain/compare/plan.ts';

export interface SelectedRunStats {
  copyCount: number;
  updateCount: number;
  moveCount: number;
  deleteCount: number;
  transferBytes: number;
  reversedCount: number;
}

export function summarizeSelectedRun(
  plan: PlanDto,
  executableIndices: number[],
  reversedRows: boolean[],
): SelectedRunStats {
  const summary: SelectedRunStats = {
    copyCount: 0,
    updateCount: 0,
    moveCount: 0,
    deleteCount: 0,
    transferBytes: 0,
    reversedCount: 0,
  };
  for (const index of executableIndices) {
    const operation = effectiveOperation(plan, reversedRows, index);
    switch (operation.action) {
      case 'copy':
        summary.copyCount++;
        summary.transferBytes += rowTransferBytes(plan, reversedRows, index);
        break;
      case 'update':
        summary.updateCount++;
        summary.transferBytes += rowTransferBytes(plan, reversedRows, index);
        break;
      case 'chmod':
        summary.updateCount++;
        break;
      case 'move':
        summary.moveCount++;
        break;
      case 'delete':
      case 'delete_dir':
        summary.deleteCount++;
        break;
    }
    if (reversedRows[index]) summary.reversedCount++;
  }
  return summary;
}
