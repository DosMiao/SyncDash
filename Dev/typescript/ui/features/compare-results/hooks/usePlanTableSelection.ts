import { useMemo } from 'react';
import { effectiveOperation, isExecutableOperation } from '#core/domain/compare/plan.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';

interface UsePlanTableSelectionProps {
  plan: PlanDto;
  reversedRows: boolean[];
  includedRows: boolean[];
  displayOrder: number[];
  inScopeIndices: number[];
}

export function usePlanTableSelection(props: UsePlanTableSelectionProps) {
  const { plan, reversedRows, includedRows, displayOrder, inScopeIndices } = props;
  // Memoize full-plan scans; virtual scrolling rerenders every frame and large index arrays are costly.
  const executableInScope = useMemo(
    () => inScopeIndices.filter(
      (index) => isExecutableOperation(effectiveOperation(plan, reversedRows, index)),
    ),
    [plan, reversedRows, inScopeIndices],
  );
  const allExecutableRowsSelectedForSync = useMemo(
    () => executableInScope.length > 0 && executableInScope.every((index) => includedRows[index]),
    [executableInScope, includedRows],
  );
  const someExecutableRowsSelectedForSync = useMemo(
    () => executableInScope.some((index) => includedRows[index]),
    [executableInScope, includedRows],
  );

  // Prefix counts make subtree selection O(1) per folder; materialize indices only on interaction.
  const synchronizationSelectionCountPrefix = useMemo(() => {
    const prefixCounts = new Uint32Array(displayOrder.length + 1);
    for (let position = 0; position < displayOrder.length; position++) {
      const index = displayOrder[position];
      prefixCounts[position + 1] = prefixCounts[position]
        + (includedRows[index]
          && isExecutableOperation(effectiveOperation(plan, reversedRows, index)) ? 1 : 0);
    }
    return prefixCounts;
  }, [plan, reversedRows, includedRows, displayOrder]);

  return {
    executableInScope,
    allExecutableRowsSelectedForSync,
    someExecutableRowsSelectedForSync,
    synchronizationSelectionCountPrefix,
  };
}
