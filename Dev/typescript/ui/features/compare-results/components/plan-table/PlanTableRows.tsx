import { PlanTableFolderRow } from './PlanTableFolderRow.tsx';
import { PlanTableOperationRow } from './PlanTableOperationRow.tsx';
import type { RefObject } from 'react';
import type { RowSpec } from '#core/domain/compare/grouping.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';
import type { ColumnDefinition, ColumnId } from '../../model/planTableColumns.ts';
import type { PlanTableRowNavigationHandler } from '../../model/planTableNavigation.ts';
import type { VirtualWindow } from '../../hooks/useVirtualRows.ts';

interface PlanTableRowsProps {
  bodySectionRef: RefObject<HTMLTableSectionElement | null>;
  rowPlan: RowSpec[];
  virtualWindow: VirtualWindow;
  rovingTabStopRowIndex: number;
  gridLabelId: string;
  columnCount: number;
  plan: PlanDto;
  reversedRows: boolean[];
  includedRows: boolean[];
  displayOrder: number[];
  pathMode: 'relative' | 'full';
  grouped: boolean;
  collapsedFolderPaths: Set<string>;
  reviewEditable: boolean;
  visibleColumnDefinitions: ColumnDefinition[];
  visibleColumnIds: Set<ColumnId>;
  synchronizationSelectionCountPrefix: Uint32Array;
  onSetRowIncluded: (index: number, value: boolean) => void;
  onSetRowsIncluded: (indices: number[], value: boolean) => void;
  onToggleRowDirection: (index: number) => void;
  onToggleFolderFold: (folderPath: string) => void;
  onContextRow: (index: number, x: number, y: number) => void;
  onActivateRow: (logicalRowIndex: number) => void;
  onNavigateRow: PlanTableRowNavigationHandler;
}

export function PlanTableRows(props: PlanTableRowsProps) {
  const {
    bodySectionRef,
    rowPlan,
    virtualWindow,
    rovingTabStopRowIndex,
    gridLabelId,
    columnCount,
    plan,
    reversedRows,
    includedRows,
    displayOrder,
    pathMode,
    grouped,
    collapsedFolderPaths,
    reviewEditable,
    visibleColumnDefinitions,
    visibleColumnIds,
    synchronizationSelectionCountPrefix,
    onSetRowIncluded,
    onSetRowsIncluded,
    onToggleRowDirection,
    onToggleFolderFold,
    onContextRow,
    onActivateRow,
    onNavigateRow,
  } = props;

  return (
    <tbody ref={bodySectionRef} role="rowgroup">
      {rowPlan.slice(virtualWindow.from, virtualWindow.to).map((row, visibleOffset) => {
        const logicalRowIndex = virtualWindow.from + visibleOffset;
        const isActiveRow = logicalRowIndex === rovingTabStopRowIndex;
        const synchronizationStatusId = `${gridLabelId}-row-${logicalRowIndex}-synchronization`;

        if (typeof row !== 'number') {
          return (
            <PlanTableFolderRow
              key={`f:${row.folderPath}`}
              row={row}
              logicalRowIndex={logicalRowIndex}
              isActiveRow={isActiveRow}
              synchronizationStatusId={synchronizationStatusId}
              columnCount={columnCount}
              plan={plan}
              reversedRows={reversedRows}
              displayOrder={displayOrder}
              reviewEditable={reviewEditable}
              collapsedFolderPaths={collapsedFolderPaths}
              synchronizationSelectionCountPrefix={synchronizationSelectionCountPrefix}
              onSetRowsIncluded={onSetRowsIncluded}
              onToggleFolderFold={onToggleFolderFold}
              onActivateRow={onActivateRow}
              onNavigateRow={onNavigateRow}
            />
          );
        }

        return (
          <PlanTableOperationRow
            key={`r:${row}`}
            index={row}
            logicalRowIndex={logicalRowIndex}
            isActiveRow={isActiveRow}
            synchronizationStatusId={synchronizationStatusId}
            hasAlternatingBackground={logicalRowIndex % 2 === 1}
            plan={plan}
            reversedRows={reversedRows}
            includedRows={includedRows}
            pathMode={pathMode}
            grouped={grouped}
            reviewEditable={reviewEditable}
            visibleColumnDefinitions={visibleColumnDefinitions}
            visibleColumnIds={visibleColumnIds}
            onSetRowIncluded={onSetRowIncluded}
            onToggleRowDirection={onToggleRowDirection}
            onContextRow={onContextRow}
            onActivateRow={onActivateRow}
            onNavigateRow={onNavigateRow}
          />
        );
      })}
    </tbody>
  );
}
