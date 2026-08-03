import { useMemo } from 'react';
import { useCssVariables } from '#ui/shared/hooks/useCssVariables.ts';
import { useContainerWidth } from './useContainerWidth.ts';
import { useVirtualRows } from './useVirtualRows.ts';
import {
  COLUMN_DEFINITIONS,
  calculateMinimumTableWidth,
  columnLayoutForWidth,
  type ColumnId,
} from '../model/planTableColumns.ts';
import type { RefObject } from 'react';
import type { RowSpec } from '#core/domain/compare/grouping.ts';
import type { CompareResultKey } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { ResultViewport } from './useVirtualRows.ts';

interface UsePlanTableCanvasProps {
  rowPlan: RowSpec[];
  scrollContainer: HTMLElement | null;
  headerSectionRef: RefObject<HTMLTableSectionElement | null>;
  bodySectionRef: RefObject<HTMLTableSectionElement | null>;
  tableCanvasRef: RefObject<HTMLDivElement | null>;
  bodyTableRef: RefObject<HTMLTableElement | null>;
  workspaceKey: CompareResultKey;
  viewport: ResultViewport;
  onViewportChange: (workspaceKey: CompareResultKey, viewport: ResultViewport) => void;
}

export function usePlanTableCanvas(props: UsePlanTableCanvasProps) {
  const {
    rowPlan,
    scrollContainer,
    headerSectionRef,
    bodySectionRef,
    tableCanvasRef,
    bodyTableRef,
    workspaceKey,
    viewport,
    onViewportChange,
  } = props;
  const virtualWindow = useVirtualRows(
    rowPlan,
    scrollContainer,
    headerSectionRef,
    bodySectionRef,
    workspaceKey,
    viewport,
    onViewportChange,
  );
  const columnLayout = columnLayoutForWidth(useContainerWidth(scrollContainer));
  const visibleColumnDefinitions = useMemo(
    () => COLUMN_DEFINITIONS.filter(
      (column) => column.widthByLayout[columnLayout] !== undefined,
    ),
    [columnLayout],
  );
  const visibleColumnIds = useMemo(
    () => new Set<ColumnId>(visibleColumnDefinitions.map((column) => column.id)),
    [visibleColumnDefinitions],
  );
  const columnCount = visibleColumnDefinitions.length;
  const minimumTableWidthPixels = calculateMinimumTableWidth(
    visibleColumnDefinitions,
    columnLayout,
  );

  useCssVariables(
    tableCanvasRef,
    {
      '--plan-table-minimum-width': `${minimumTableWidthPixels}px`,
      '--plan-table-canvas-height': `${virtualWindow.canvasHeight}px`,
    },
    [minimumTableWidthPixels, virtualWindow.canvasHeight],
  );

  useCssVariables(
    bodyTableRef,
    { '--plan-table-body-top': `${virtualWindow.bodyTop}px` },
    [virtualWindow.bodyTop],
  );

  return {
    virtualWindow,
    columnLayout,
    visibleColumnDefinitions,
    visibleColumnIds,
    columnCount,
  };
}
