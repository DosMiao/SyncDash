import { owningFolderOf, ROOT_FOLDER_PATH } from '#core/domain/compare/folders.ts';
import { effectiveOperation } from '#core/domain/compare/plan.ts';
import type { KeyboardEvent as ReactKeyboardEvent } from 'react';
import type { RowSpec } from '#core/domain/compare/grouping.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';

export type PlanTableRowNavigationHandler = (
  event: ReactKeyboardEvent<HTMLTableRowElement>,
  logicalRowIndex: number,
) => boolean;

export function planRowIdentity(row: RowSpec): string {
  return typeof row === 'number' ? `operation:${row}` : `folder:${row.folderPath}`;
}

export function treeLevelForPlanRow(
  rowPlan: RowSpec[],
  logicalRowIndex: number,
  grouped: boolean,
  plan: PlanDto,
  reversedRows: boolean[],
): number | null {
  if (!grouped) return null;
  const row = rowPlan[logicalRowIndex];
  if (row === undefined) return null;
  if (typeof row !== 'number') return row.depth + 1;
  const folderPath = owningFolderOf(effectiveOperation(plan, reversedRows, row));
  return folderPath === ROOT_FOLDER_PATH ? 2 : folderPath.split('/').length + 1;
}

export function parentPlanRowIndex(
  rowPlan: RowSpec[],
  logicalRowIndex: number,
  grouped: boolean,
  plan: PlanDto,
  reversedRows: boolean[],
): number | null {
  const currentLevel = treeLevelForPlanRow(
    rowPlan,
    logicalRowIndex,
    grouped,
    plan,
    reversedRows,
  );
  if (currentLevel === null || currentLevel <= 1) return null;
  for (let candidateIndex = logicalRowIndex - 1; candidateIndex >= 0; candidateIndex--) {
    if (treeLevelForPlanRow(rowPlan, candidateIndex, grouped, plan, reversedRows)
      === currentLevel - 1
    ) return candidateIndex;
  }
  return null;
}
