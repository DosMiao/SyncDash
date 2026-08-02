import { usePlanTableController } from '#ui/features/compare-results/hooks/usePlanTableController.ts';
import { PlanTableHeader } from './plan-table/PlanTableHeader.tsx';
import { PlanTableColumnGroup } from './plan-table/PlanTablePrimitives.tsx';
import { PlanTableRows } from './plan-table/PlanTableRows.tsx';
import type { RowSpec } from '#core/domain/compare/grouping.ts';
import type { PlanDto, SortKey, Sort } from '#core/domain/compare/plan.ts';
import type { CompareResultKey } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { ResultViewport } from '#ui/features/compare-results/hooks/useVirtualRows.ts';

interface PlanTableProps {
  plan: PlanDto;
  reversedRows: boolean[];
  includedRows: boolean[];
  rowPlan: RowSpec[];
  // Includes collapsed descendants so folder ranges and CSV remain complete.
  displayOrder: number[];
  inScopeIndices: number[];
  pathMode: 'relative' | 'full';
  grouped: boolean;
  sort: Sort | null;
  collapsedFolderPaths: Set<string>;
  workspaceKey: CompareResultKey;
  viewport: ResultViewport;
  reviewEditable: boolean;
  // The caller-owned scroll container controls virtual and responsive geometry.
  wrap: HTMLElement | null;
  onSetRowIncluded: (index: number, value: boolean) => void;
  onSetRowsIncluded: (indices: number[], value: boolean) => void;
  onToggleRowDirection: (index: number) => void;
  onToggleFolderFold: (folderPath: string) => void;
  onSort: (key: SortKey) => void;
  onContextRow: (index: number, x: number, y: number) => void;
  onViewportChange: (workspaceKey: CompareResultKey, viewport: ResultViewport) => void;
}

export function PlanTable(props: PlanTableProps) {
  const {
    plan,
    reversedRows,
    includedRows,
    rowPlan,
    displayOrder,
    inScopeIndices,
    pathMode,
    grouped,
    sort: activeSort,
    collapsedFolderPaths,
    workspaceKey,
    viewport,
    reviewEditable,
    wrap: scrollContainer,
    onSetRowIncluded,
    onSetRowsIncluded,
    onToggleRowDirection,
    onToggleFolderFold,
    onSort,
    onContextRow,
    onViewportChange,
  } = props;
  const table = usePlanTableController({
    plan,
    reversedRows,
    includedRows,
    rowPlan,
    displayOrder,
    inScopeIndices,
    grouped,
    collapsedFolderPaths,
    workspaceKey,
    viewport,
    scrollContainer,
    onToggleFolderFold,
    onViewportChange,
  });

  // Keep logical heights in useVirtualRows; CSS receives only bounded physical canvas coordinates.
  return (
    <div
      ref={table.tableCanvasRef}
      className="plan-table-canvas"
      role={grouped ? 'treegrid' : 'grid'}
      aria-labelledby={table.gridLabelId}
      aria-describedby={table.gridInstructionsId}
      aria-rowcount={rowPlan.length + 1}
      aria-colcount={table.columnCount}
      aria-multiselectable="true"
    >
      <span id={table.gridLabelId} className="sr-only">Synchronization result review</span>
      <span id={table.gridInstructionsId} className="sr-only">
        Sync checkboxes select in-scope executable actions for Synchronize. Use Up and Down Arrow
        to move between rows, Space to change the current row’s Synchronize selection, Tab to reach
        its controls, and Shift F10 to open an operation’s context menu.
      </span>
      <PlanTableHeader
        headerSectionRef={table.headerSectionRef}
        columns={table.visibleColumnDefinitions}
        columnLayout={table.columnLayout}
        activeSort={activeSort}
        reviewEditable={reviewEditable}
        executableInScope={table.executableInScope}
        allExecutableRowsSelectedForSync={table.allExecutableRowsSelectedForSync}
        someExecutableRowsSelectedForSync={table.someExecutableRowsSelectedForSync}
        onSetRowsIncluded={onSetRowsIncluded}
        onSort={onSort}
      />
      <table
        ref={table.bodyTableRef}
        className="plan-table plan-table-body"
        role="presentation"
      >
        <PlanTableColumnGroup
          columns={table.visibleColumnDefinitions}
          layout={table.columnLayout}
        />
        <PlanTableRows
          bodySectionRef={table.bodySectionRef}
          rowPlan={rowPlan}
          virtualWindow={table.virtualWindow}
          rovingTabStopRowIndex={table.rovingTabStopRowIndex}
          gridLabelId={table.gridLabelId}
          columnCount={table.columnCount}
          plan={plan}
          reversedRows={reversedRows}
          includedRows={includedRows}
          displayOrder={displayOrder}
          pathMode={pathMode}
          grouped={grouped}
          collapsedFolderPaths={collapsedFolderPaths}
          reviewEditable={reviewEditable}
          visibleColumnDefinitions={table.visibleColumnDefinitions}
          visibleColumnIds={table.visibleColumnIds}
          synchronizationSelectionCountPrefix={table.synchronizationSelectionCountPrefix}
          onSetRowIncluded={onSetRowIncluded}
          onSetRowsIncluded={onSetRowsIncluded}
          onToggleRowDirection={onToggleRowDirection}
          onToggleFolderFold={onToggleFolderFold}
          onContextRow={onContextRow}
          onActivateRow={table.activatePlanRow}
          onNavigateRow={table.handleRowNavigation}
        />
      </table>
    </div>
  );
}
