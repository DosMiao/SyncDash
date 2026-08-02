import { useId, useRef } from 'react';
import { usePlanTableCanvas } from './usePlanTableCanvas.ts';
import { usePlanTableFocusState, usePlanTableNavigation } from './usePlanTableNavigation.ts';
import { usePlanTableSelection } from './usePlanTableSelection.ts';
import type { RowSpec } from '#core/domain/compare/grouping.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';
import type { CompareResultKey } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { ResultViewport } from './useVirtualRows.ts';

interface UsePlanTableControllerProps {
  plan: PlanDto;
  reversedRows: boolean[];
  includedRows: boolean[];
  rowPlan: RowSpec[];
  displayOrder: number[];
  inScopeIndices: number[];
  grouped: boolean;
  collapsedFolderPaths: Set<string>;
  workspaceKey: CompareResultKey;
  viewport: ResultViewport;
  scrollContainer: HTMLElement | null;
  onToggleFolderFold: (folderPath: string) => void;
  onViewportChange: (workspaceKey: CompareResultKey, viewport: ResultViewport) => void;
}

export function usePlanTableController(props: UsePlanTableControllerProps) {
  const {
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
  } = props;
  const headerSectionRef = useRef<HTMLTableSectionElement>(null);
  const bodySectionRef = useRef<HTMLTableSectionElement>(null);
  const tableCanvasRef = useRef<HTMLDivElement>(null);
  const bodyTableRef = useRef<HTMLTableElement>(null);
  const gridLabelId = useId();
  const gridInstructionsId = useId();
  const focusState = usePlanTableFocusState(rowPlan, workspaceKey);
  const canvas = usePlanTableCanvas({
    rowPlan,
    scrollContainer,
    headerSectionRef,
    bodySectionRef,
    tableCanvasRef,
    bodyTableRef,
    workspaceKey,
    viewport,
    onViewportChange,
  });
  const navigation = usePlanTableNavigation({
    focusState,
    plan,
    reversedRows,
    rowPlan,
    grouped,
    collapsedFolderPaths,
    workspaceKey,
    scrollContainer,
    tableCanvasRef,
    virtualWindow: canvas.virtualWindow,
    onToggleFolderFold,
  });
  const selection = usePlanTableSelection({
    plan,
    reversedRows,
    includedRows,
    displayOrder,
    inScopeIndices,
  });

  return {
    headerSectionRef,
    bodySectionRef,
    tableCanvasRef,
    bodyTableRef,
    gridLabelId,
    gridInstructionsId,
    ...canvas,
    ...navigation,
    ...selection,
  };
}
