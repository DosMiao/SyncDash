import { useLayoutEffect, useMemo, useRef, useState } from 'react';
import {
  parentPlanRowIndex,
  planRowIdentity,
  treeLevelForPlanRow,
  type PlanTableRowNavigationHandler,
} from '../model/planTableNavigation.ts';
import type { Dispatch, RefObject, SetStateAction } from 'react';
import type { RowSpec } from '#core/domain/compare/grouping.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';
import type { CompareResultKey } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { VirtualWindow } from './useVirtualRows.ts';

interface ActivePlanRow {
  workspaceKey: CompareResultKey;
  logicalRowIndex: number;
  identity: string;
}

interface PendingPlanRowFocus {
  workspaceKey: CompareResultKey;
  logicalRowIndex: number;
}

export interface PlanTableFocusState {
  activePlanRow: ActivePlanRow | null;
  setActivePlanRow: Dispatch<SetStateAction<ActivePlanRow | null>>;
  pendingPlanRowFocus: { current: PendingPlanRowFocus | null };
}

export function usePlanTableFocusState(
  rowPlan: RowSpec[],
  workspaceKey: CompareResultKey,
): PlanTableFocusState {
  const [activePlanRow, setActivePlanRow] = useState<ActivePlanRow | null>(() => (
    rowPlan[0] === undefined
      ? null
      : { workspaceKey, logicalRowIndex: 0, identity: planRowIdentity(rowPlan[0]) }
  ));
  const pendingPlanRowFocus = useRef<PendingPlanRowFocus | null>(null);
  return { activePlanRow, setActivePlanRow, pendingPlanRowFocus };
}

interface UsePlanTableNavigationProps {
  focusState: PlanTableFocusState;
  plan: PlanDto;
  reversedRows: boolean[];
  rowPlan: RowSpec[];
  grouped: boolean;
  collapsedFolderPaths: Set<string>;
  workspaceKey: CompareResultKey;
  scrollContainer: HTMLElement | null;
  tableCanvasRef: RefObject<HTMLDivElement | null>;
  virtualWindow: VirtualWindow;
  onToggleFolderFold: (folderPath: string) => void;
}

export function usePlanTableNavigation(props: UsePlanTableNavigationProps) {
  const {
    focusState,
    plan,
    reversedRows,
    rowPlan,
    grouped,
    collapsedFolderPaths,
    workspaceKey,
    scrollContainer,
    tableCanvasRef,
    virtualWindow,
    onToggleFolderFold,
  } = props;
  const { activePlanRow, setActivePlanRow, pendingPlanRowFocus } = focusState;
  const requestedActiveRowIndex = useMemo(() => {
    if (!activePlanRow || activePlanRow.workspaceKey !== workspaceKey || rowPlan.length === 0) return 0;
    const rowAtRequestedIndex = rowPlan[activePlanRow.logicalRowIndex];
    if (rowAtRequestedIndex !== undefined
      && planRowIdentity(rowAtRequestedIndex) === activePlanRow.identity
    ) {
      return activePlanRow.logicalRowIndex;
    }
    const relocatedIndex = rowPlan.findIndex((row) => planRowIdentity(row) === activePlanRow.identity);
    return relocatedIndex < 0 ? 0 : relocatedIndex;
  }, [activePlanRow, rowPlan, workspaceKey]);
  const rovingTabStopRowIndex = requestedActiveRowIndex >= virtualWindow.from
    && requestedActiveRowIndex < virtualWindow.to
    ? requestedActiveRowIndex
    : virtualWindow.from;

  const activatePlanRow = (logicalRowIndex: number) => {
    const row = rowPlan[logicalRowIndex];
    if (row === undefined) return;
    const nextActiveRow = {
      workspaceKey,
      logicalRowIndex,
      identity: planRowIdentity(row),
    };
    setActivePlanRow((current) => (
      current?.workspaceKey === nextActiveRow.workspaceKey
        && current.logicalRowIndex === nextActiveRow.logicalRowIndex
        && current.identity === nextActiveRow.identity
        ? current
        : nextActiveRow
    ));
  };

  const renderedRowElement = (logicalRowIndex: number): HTMLTableRowElement | null => (
    tableCanvasRef.current?.querySelector<HTMLTableRowElement>(
      `tr[data-plan-logical-row="${logicalRowIndex}"]`,
    ) ?? null
  );

  const requestPlanRowFocus = (logicalRowIndex: number) => {
    if (rowPlan.length === 0) return;
    const boundedIndex = Math.max(0, Math.min(logicalRowIndex, rowPlan.length - 1));
    activatePlanRow(boundedIndex);
    pendingPlanRowFocus.current = { workspaceKey, logicalRowIndex: boundedIndex };
    const renderedRow = renderedRowElement(boundedIndex);
    if (renderedRow) {
      pendingPlanRowFocus.current = null;
      renderedRow.focus({ preventScroll: true });
      renderedRow.scrollIntoView({ block: 'nearest', inline: 'nearest' });
      return;
    }
    if (!scrollContainer) return;
    const maximumScrollTop = Math.max(0, scrollContainer.scrollHeight - scrollContainer.clientHeight);
    scrollContainer.scrollTop = rowPlan.length === 1
      ? 0
      : Math.round(maximumScrollTop * boundedIndex / (rowPlan.length - 1));
  };

  useLayoutEffect(() => {
    const pendingFocus = pendingPlanRowFocus.current;
    if (!pendingFocus || pendingFocus.workspaceKey !== workspaceKey) {
      pendingPlanRowFocus.current = null;
      return;
    }
    const renderedRow = renderedRowElement(pendingFocus.logicalRowIndex);
    if (renderedRow) {
      pendingPlanRowFocus.current = null;
      renderedRow.focus({ preventScroll: true });
      renderedRow.scrollIntoView({ block: 'nearest', inline: 'nearest' });
      return;
    }
    if (!scrollContainer) return;
    const maximumScrollTop = Math.max(0, scrollContainer.scrollHeight - scrollContainer.clientHeight);
    const direction = pendingFocus.logicalRowIndex < virtualWindow.from ? -1 : 1;
    const nextScrollTop = Math.max(
      0,
      Math.min(maximumScrollTop, scrollContainer.scrollTop + direction * scrollContainer.clientHeight),
    );
    if (nextScrollTop === scrollContainer.scrollTop) {
      pendingPlanRowFocus.current = null;
      return;
    }
    scrollContainer.scrollTop = nextScrollTop;
  }, [pendingPlanRowFocus, rowPlan, scrollContainer, tableCanvasRef, virtualWindow.from, virtualWindow.to, workspaceKey]);

  const handleRowNavigation: PlanTableRowNavigationHandler = (event, logicalRowIndex) => {
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return false;
    const row = rowPlan[logicalRowIndex];
    if (row === undefined) return false;
    let targetRowIndex: number | null = null;
    if (event.key === 'ArrowUp') targetRowIndex = logicalRowIndex - 1;
    else if (event.key === 'ArrowDown') targetRowIndex = logicalRowIndex + 1;
    else if (event.key === 'Home') targetRowIndex = 0;
    else if (event.key === 'End') targetRowIndex = rowPlan.length - 1;
    else if (grouped && event.key === 'ArrowLeft') {
      if (typeof row !== 'number' && !collapsedFolderPaths.has(row.folderPath)) {
        event.preventDefault();
        event.stopPropagation();
        onToggleFolderFold(row.folderPath);
        return true;
      }
      targetRowIndex = parentPlanRowIndex(
        rowPlan,
        logicalRowIndex,
        grouped,
        plan,
        reversedRows,
      );
    } else if (grouped && event.key === 'ArrowRight' && typeof row !== 'number') {
      if (collapsedFolderPaths.has(row.folderPath)) {
        event.preventDefault();
        event.stopPropagation();
        onToggleFolderFold(row.folderPath);
        return true;
      }
      const nextRowLevel = treeLevelForPlanRow(
        rowPlan,
        logicalRowIndex + 1,
        grouped,
        plan,
        reversedRows,
      );
      if (nextRowLevel !== null && nextRowLevel > row.depth + 1) {
        targetRowIndex = logicalRowIndex + 1;
      }
    }
    if (targetRowIndex === null
      || targetRowIndex < 0
      || targetRowIndex >= rowPlan.length
    ) return false;
    event.preventDefault();
    event.stopPropagation();
    requestPlanRowFocus(targetRowIndex);
    return true;
  };

  return { rovingTabStopRowIndex, activatePlanRow, handleRowNavigation };
}
