import { useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { ChevronDown, ChevronRight, ChevronUp, Folder, FolderOpen } from 'lucide-react';
import {
  formatFileTimestamp,
  humanSize,
  joinDisplayPath,
  parentRelativePath,
  relativePathBaseName,
} from '../../core/format';
import { owningFolderOf, ROOT_FOLDER_PATH, ROOT_LEVEL_LABEL } from '../../core/folders';
import {
  canReverseOperation,
  effectiveOperation,
  newerSide,
  describeRowAction,
  rowMetadata,
  isExecutableOperation,
  sidePaths,
} from '../../core/plan';
import { DIRECTION_ICON, RESULT_TYPE_ICON } from '../icons';
import { useVirtualRows } from '../hooks/useVirtualRows';
import type { ResultViewport } from '../hooks/useVirtualRows';
import type { CompareResultKey } from '../state/compareWorkspaceModel';
import type { ComponentPropsWithoutRef, KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';
import type { RowSpec } from '../../core/grouping';
import type { PlanDto, SortKey, Sort } from '../../core/plan';
import type { SideMeta } from '../../core/types/generated/SideMeta';

interface PlanTableProps {
  plan: PlanDto;
  reversedRows: boolean[];
  includedRows: boolean[];
  rowPlan: RowSpec[];
  // Collapsed descendants remain in this display order so folder ranges and CSV stay complete.
  displayOrder: number[];
  inScopeIndices: number[];
  pathMode: 'relative' | 'full';
  grouped: boolean;
  sort: Sort | null;
  collapsedFolderPaths: Set<string>;
  workspaceKey: CompareResultKey;
  viewport: ResultViewport;
  reviewEditable: boolean;
  // Both virtualization and responsive columns measure the caller-owned scroll container.
  wrap: HTMLElement | null;
  onSetRowIncluded: (index: number, value: boolean) => void;
  onSetRowsIncluded: (indices: number[], value: boolean) => void;
  onToggleRowDirection: (index: number) => void;
  onToggleFolderFold: (folderPath: string) => void;
  onSort: (key: SortKey) => void;
  onContextRow: (index: number, x: number, y: number) => void;
  onViewportChange: (workspaceKey: CompareResultKey, viewport: ResultViewport) => void;
}

type ColumnLayout =
  | 'allColumns'
  | 'withoutReason'
  | 'withoutReasonOrTimestamps'
  | 'synchronizePathsAndAction';

/// A column's identity **is** its sort key — there is no column you can sort by two ways, and no key
/// without a column. Only the Synchronize selection has neither.
type ColumnId = 'synchronize' | SortKey;
type TableSide = 'source' | 'target';

const SIDE_COLUMN_IDS = {
  source: { path: 's.path', size: 's.size', timestamp: 's.mtime' },
  target: { path: 't.path', size: 't.size', timestamp: 't.mtime' },
} as const satisfies Record<TableSide, Record<'path' | 'size' | 'timestamp', SortKey>>;

interface ColumnDefinition {
  id: ColumnId;
  /// Keys this header takes over in layouts where their own column is gone. A column that drops
  /// must not take its sort key with it: the header that owns the key would then be unmounted in
  /// exactly the layout where it is the last place left to click, leaving a stale sort you can see but
  /// not change.
  adoptedSortKeys?: Partial<Record<ColumnLayout, SortKey[]>>;
  /// CSS-pixel width per layout. An absent layout omits the column; null leaves a path column flexible.
  widthByLayout: Partial<Record<ColumnLayout, number | null>>;
  className: string;
  title?: string;
  editableTitle?: string;
}

/// Header text. Short and contextual, because the column it sits over says which side it is;
/// SORT_LABEL in core/plan.ts carries the unambiguous name for the "sorted by" indicator.
const COLUMN_HEADER_LABELS: Record<SortKey, string> = {
  's.path': 'source', 't.path': 'target', action: 'action',
  's.size': 'size', 't.size': 'size', 's.mtime': 'time', 't.mtime': 'time',
  reason: 'reason',
};

/// The table's whole layout, in order. The row reads left to right as source facts → what happens →
/// target facts, so the action sits on the axis between the two sides rather than in front of them,
/// and each side's size and time are their own columns rather than one fused cell.
///
/// One descriptor per column drives the <colgroup>, the <thead> and the row cells. The alternative —
/// a width array per layout plus `layout === 'allColumns' &&` guards repeated in the header and the
/// body — is a dozen places that have to agree, and under `table-layout: fixed` a disagreement is
/// *silent*:
/// surplus <col>s are ignored and missing ones make the trailing columns split the remainder, so the
/// failure reads as "the widths went odd at one window size" and nobody bisects it.
///
/// The 1240→1000 step is exactly the reason column's 240px, so crossing it does not take width from
/// the two primary path columns.
const COLUMN_DEFINITIONS: ColumnDefinition[] = [
  {
    id: 'synchronize',
    className: 'c-synchronize',
    widthByLayout: {
      allColumns: 58,
      withoutReason: 58,
      withoutReasonOrTimestamps: 58,
      synchronizePathsAndAction: 58,
    },
  },
  {
    id: 's.path', className: 'c-path c-source-path',
    adoptedSortKeys: { synchronizePathsAndAction: ['s.size', 's.mtime'] },
    widthByLayout: {
      allColumns: null,
      withoutReason: null,
      withoutReasonOrTimestamps: null,
      synchronizePathsAndAction: null,
    },
  },
  // 112 rather than 92 without timestamps: an ellipsized "size · ti…" would leave the time
  // span unclickable, and the table header clips. 92 fits "894.0 MB"; 112 fits the composite header.
  {
    id: 's.size', className: 'c-size',
    adoptedSortKeys: { withoutReasonOrTimestamps: ['s.mtime'] },
    widthByLayout: { allColumns: 92, withoutReason: 92, withoutReasonOrTimestamps: 112 },
  },
  { id: 's.mtime', className: 'c-time', widthByLayout: { allColumns: 136, withoutReason: 136 } },
  {
    id: 'action', className: 'c-action',
    widthByLayout: {
      allColumns: 124,
      withoutReason: 124,
      withoutReasonOrTimestamps: 124,
      synchronizePathsAndAction: 124,
    },
    title: 'Sort by action.',
    editableTitle: "Sort by action. Activate a row's action to reverse its direction; activate it again to restore.",
  },
  {
    id: 't.path', className: 'c-path c-target-path',
    adoptedSortKeys: { synchronizePathsAndAction: ['t.size', 't.mtime'] },
    widthByLayout: {
      allColumns: null,
      withoutReason: null,
      withoutReasonOrTimestamps: null,
      synchronizePathsAndAction: null,
    },
  },
  {
    id: 't.size', className: 'c-size',
    adoptedSortKeys: { withoutReasonOrTimestamps: ['t.mtime'] },
    widthByLayout: { allColumns: 92, withoutReason: 92, withoutReasonOrTimestamps: 112 },
  },
  { id: 't.mtime', className: 'c-time', widthByLayout: { allColumns: 136, withoutReason: 136 } },
  { id: 'reason', className: 'c-reason', widthByLayout: { allColumns: 240 } },
];

const columnLayoutForWidth = (containerWidthPixels: number): ColumnLayout => (
  containerWidthPixels >= 1240
    ? 'allColumns'
    : containerWidthPixels >= 1000
      ? 'withoutReason'
      : containerWidthPixels >= 700
        ? 'withoutReasonOrTimestamps'
        : 'synchronizePathsAndAction'
);

/// The narrowest a path column may be before the table stops shrinking and scrolls sideways instead.
/// Under fixed layout the columns with no <col> width absorb every shortfall, so without a floor they
/// go to zero and the paths vanish entirely rather than the table admitting it has run out of room.
const MINIMUM_PATH_COLUMN_WIDTH = 140;

function columnWidthForLayout(
  column: ColumnDefinition,
  layout: ColumnLayout,
): number | null {
  const width = column.widthByLayout[layout];
  if (width === undefined) {
    throw new Error(`Column ${column.id} is not available in layout ${layout}`);
  }
  return width;
}

/// Minimum table width for a column set: everything pinned, plus a floor for each path column. A
/// static number cannot do this job — the pinned total is 878 with all columns and 182 with only
/// Synchronize selection, paths, and action, so one value is either too wide for the narrow set or no constraint
/// for the wide one.
function calculateMinimumTableWidth(columns: ColumnDefinition[], layout: ColumnLayout): number {
  let fixedWidth = 0;
  let flexibleColumnCount = 0;
  for (const column of columns) {
    const width = columnWidthForLayout(column, layout);
    if (width === null) flexibleColumnCount++; else fixedWidth += width;
  }
  return fixedWidth + flexibleColumnCount * MINIMUM_PATH_COLUMN_WIDTH;
}

/// The column set tracks the scroll container because collapsing Run Scope changes that width
/// without resizing the window.
function useContainerWidth(container: HTMLElement | null): number {
  const [containerWidthPixels, setContainerWidthPixels] = useState(1600);
  useLayoutEffect(() => {
    if (!container) return;
    const observer = new ResizeObserver(() => setContainerWidthPixels(container.clientWidth));
    observer.observe(container);
    setContainerWidthPixels(container.clientWidth);
    return () => observer.disconnect();
  }, [container]);
  return containerWidthPixels;
}

/// A clickable header. Declared at module scope, not inside PlanTable: a component defined in a
/// render body is a *new type* on every render, so React unmounts and rebuilds every one of these
/// controls — and this table re-renders on every scroll frame.
function SortHeader(props: { sortKey: SortKey; sort: Sort | null; onSort: (key: SortKey) => void }) {
  const { sortKey, sort, onSort } = props;
  const isActiveSort = sort?.key === sortKey;
  const currentSortAnnouncement = isActiveSort
    ? `, currently ${sort.dir === 1 ? 'ascending' : 'descending'}`
    : '';
  return (
    <button
      type="button"
      className={'sort-button' + (isActiveSort ? ' on' : '')}
      aria-pressed={isActiveSort}
      aria-label={`Sort by ${COLUMN_HEADER_LABELS[sortKey]}${currentSortAnnouncement}`}
      onClick={() => onSort(sortKey)}
    >
      {COLUMN_HEADER_LABELS[sortKey]}
      {isActiveSort && (
        <span className="sort-indicator">
          {sort.dir === 1 ? <ChevronUp size={11} /> : <ChevronDown size={11} />}
        </span>
      )}
    </button>
  );
}

/// `indeterminate` is a DOM property with no HTML attribute, so React cannot declare it in JSX.
function IndeterminateCheckbox(props: {
  checked: boolean;
  indeterminate?: boolean;
  disabled?: boolean;
  title?: string;
  ariaLabel: string;
  tabIndex?: number;
  onChange: (value: boolean) => void;
}) {
  const checkboxRef = useRef<HTMLInputElement>(null);
  useLayoutEffect(() => {
    const checkbox = checkboxRef.current;
    if (!checkbox) return;
    checkbox.indeterminate = !!props.indeterminate;
    return () => { checkbox.indeterminate = false; };
  }, [props.indeterminate]);
  return (
    <input
      ref={checkboxRef}
      type="checkbox"
      checked={props.checked}
      disabled={props.disabled}
      title={props.title}
      aria-label={props.ariaLabel}
      tabIndex={props.tabIndex}
      onChange={(event) => props.onChange(event.target.checked)}
    />
  );
}

/// What a column contributes to one row. The <td> itself is emitted by the single loop below, so
/// the cell count can never disagree with the <col> count.
interface TableCell { className?: string; title?: string; children?: ReactNode }

/// Both metadata cells carry the whole truth in their tooltip regardless of layout, so dropping the
/// time column does not drop its information.
const formatMetadataTitle = (metadata: SideMeta) => (
  `${metadata.size.toLocaleString()} bytes\n${new Date(metadata.mtime_ms).toLocaleString()}`
);
const ABSENT_METADATA_CELL: TableCell = { className: 'mono dim', children: '—' };

function buildSizeCell(metadata: SideMeta | null, highlighted: boolean): TableCell {
  if (!metadata) return ABSENT_METADATA_CELL;
  return {
    className: 'mono' + (highlighted ? ' newer' : ''),
    title: formatMetadataTitle(metadata),
    children: humanSize(metadata.size),
  };
}

function buildTimestampCell(metadata: SideMeta | null, highlighted: boolean): TableCell {
  if (!metadata) return ABSENT_METADATA_CELL;
  return {
    className: 'mono' + (highlighted ? ' newer' : ''),
    title: formatMetadataTitle(metadata),
    children: formatFileTimestamp(metadata.mtime_ms),
  };
}

function PlanTableColumn(props: { widthPixels: number | null }) {
  const { widthPixels } = props;
  const columnRef = useRef<HTMLTableColElement>(null);
  useLayoutEffect(() => {
    const column = columnRef.current;
    if (!column) return;
    if (widthPixels === null) {
      column.style.removeProperty('--plan-table-column-width');
    } else {
      column.style.setProperty('--plan-table-column-width', `${widthPixels}px`);
    }
    return () => { column.style.removeProperty('--plan-table-column-width'); };
  }, [widthPixels]);
  return <col ref={columnRef} className="plan-table-column" />;
}

function PlanTableColumnGroup(props: { columns: ColumnDefinition[]; layout: ColumnLayout }) {
  const { columns, layout } = props;
  return (
    <colgroup>
      {columns.map((column) => {
        const widthPixels = columnWidthForLayout(column, layout);
        return <PlanTableColumn key={column.id} widthPixels={widthPixels} />;
      })}
    </colgroup>
  );
}

type TreeDepthTableRowProps = Omit<ComponentPropsWithoutRef<'tr'>, 'style'> & {
  treeDepth?: number;
};

function TreeDepthTableRow(props: TreeDepthTableRowProps) {
  const { treeDepth, ...rowProps } = props;
  const tableRowRef = useRef<HTMLTableRowElement>(null);
  useLayoutEffect(() => {
    const tableRow = tableRowRef.current;
    if (!tableRow) return;
    if (treeDepth === undefined) {
      tableRow.style.removeProperty('--tree-depth');
    } else {
      tableRow.style.setProperty('--tree-depth', String(treeDepth));
    }
    return () => { tableRow.style.removeProperty('--tree-depth'); };
  }, [treeDepth]);
  return <tr ref={tableRowRef} {...rowProps} />;
}

function planRowIdentity(row: RowSpec): string {
  return typeof row === 'number' ? `operation:${row}` : `folder:${row.folderPath}`;
}

interface ActivePlanRow {
  workspaceKey: CompareResultKey;
  logicalRowIndex: number;
  identity: string;
}

interface PendingPlanRowFocus {
  workspaceKey: CompareResultKey;
  logicalRowIndex: number;
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

  const headerSectionRef = useRef<HTMLTableSectionElement>(null);
  const bodySectionRef = useRef<HTMLTableSectionElement>(null);
  const tableCanvasRef = useRef<HTMLDivElement>(null);
  const bodyTableRef = useRef<HTMLTableElement>(null);
  const gridLabelId = useId();
  const gridInstructionsId = useId();
  const [activePlanRow, setActivePlanRow] = useState<ActivePlanRow | null>(() => (
    rowPlan[0] === undefined
      ? null
      : { workspaceKey, logicalRowIndex: 0, identity: planRowIdentity(rowPlan[0]) }
  ));
  const pendingPlanRowFocus = useRef<PendingPlanRowFocus | null>(null);

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

  useLayoutEffect(() => {
    const tableCanvas = tableCanvasRef.current;
    if (!tableCanvas) return;
    tableCanvas.style.setProperty('--plan-table-minimum-width', `${minimumTableWidthPixels}px`);
    tableCanvas.style.setProperty('--plan-table-canvas-height', `${virtualWindow.canvasHeight}px`);
    return () => {
      tableCanvas.style.removeProperty('--plan-table-minimum-width');
      tableCanvas.style.removeProperty('--plan-table-canvas-height');
    };
  }, [minimumTableWidthPixels, virtualWindow.canvasHeight]);

  useLayoutEffect(() => {
    const bodyTable = bodyTableRef.current;
    if (!bodyTable) return;
    bodyTable.style.setProperty('--plan-table-body-top', `${virtualWindow.bodyTop}px`);
    return () => { bodyTable.style.removeProperty('--plan-table-body-top'); };
  }, [virtualWindow.bodyTop]);

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
  }, [rowPlan, scrollContainer, virtualWindow.from, virtualWindow.to, workspaceKey]);

  // Scrolling updates this component's local virtual-window state every animation frame. These two
  // full-plan passes used to run on every one of those renders; at several hundred thousand rows,
  // trackpad momentum allocated another multi-megabyte index array per frame until WebKit hit
  // memory pressure and painted the window black.
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

  // A folder row needs the Synchronize-selected count of an arbitrary subtree. Prefixing the one
  // DFS order once makes that O(1) per rendered folder, instead of repeatedly scanning a 100k-file
  // parent on every virtual-scroll render. Descendant indices themselves are materialized only
  // when a checkbox is actually clicked.
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

  // Newness is a timestamp claim. When the responsive layout removes that column, carry its cue to
  // size and then path so the evidence remains visible in every layout.
  const highlightColumnForSide = (side: TableSide): ColumnId => {
    const sideColumns = SIDE_COLUMN_IDS[side];
    if (visibleColumnIds.has(sideColumns.timestamp)) return sideColumns.timestamp;
    if (visibleColumnIds.has(sideColumns.size)) return sideColumns.size;
    return sideColumns.path;
  };

  const treeLevelForRow = (logicalRowIndex: number): number | null => {
    if (!grouped) return null;
    const row = rowPlan[logicalRowIndex];
    if (row === undefined) return null;
    if (typeof row !== 'number') return row.depth + 1;
    const folderPath = owningFolderOf(effectiveOperation(plan, reversedRows, row));
    return folderPath === ROOT_FOLDER_PATH ? 2 : folderPath.split('/').length + 1;
  };

  const parentRowIndex = (logicalRowIndex: number): number | null => {
    const currentLevel = treeLevelForRow(logicalRowIndex);
    if (currentLevel === null || currentLevel <= 1) return null;
    for (let candidateIndex = logicalRowIndex - 1; candidateIndex >= 0; candidateIndex--) {
      if (treeLevelForRow(candidateIndex) === currentLevel - 1) return candidateIndex;
    }
    return null;
  };

  const handleRowNavigation = (
    event: ReactKeyboardEvent<HTMLTableRowElement>,
    logicalRowIndex: number,
  ): boolean => {
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
      targetRowIndex = parentRowIndex(logicalRowIndex);
    } else if (grouped && event.key === 'ArrowRight' && typeof row !== 'number') {
      if (collapsedFolderPaths.has(row.folderPath)) {
        event.preventDefault();
        event.stopPropagation();
        onToggleFolderFold(row.folderPath);
        return true;
      }
      const nextRowLevel = treeLevelForRow(logicalRowIndex + 1);
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

  const tableBody = (
    <tbody ref={bodySectionRef} role="rowgroup">
        {rowPlan.slice(virtualWindow.from, virtualWindow.to).map((row, visibleOffset) => {
          const logicalRowIndex = virtualWindow.from + visibleOffset;
          const isActiveRow = logicalRowIndex === rovingTabStopRowIndex;
          const synchronizationStatusId = `${gridLabelId}-row-${logicalRowIndex}-synchronization`;
          const hasAlternatingBackground = logicalRowIndex % 2 === 1;

          if (typeof row !== 'number') {
            const { bytes } = row;
            const selectedForSyncCount = synchronizationSelectionCountPrefix[row.end]
              - synchronizationSelectionCountPrefix[row.start];
            const allFolderActionsSelected = row.executableCount > 0
              && selectedForSyncCount === row.executableCount;
            const someFolderActionsSelected = selectedForSyncCount > 0
              && selectedForSyncCount < row.executableCount;
            const isFolderFolded = collapsedFolderPaths.has(row.folderPath);
            const isRootFolder = row.folderPath === ROOT_FOLDER_PATH;
            const folderLabel = isRootFolder ? ROOT_LEVEL_LABEL : relativePathBaseName(row.folderPath);
            const toggleFolderSelection = (value: boolean) => {
              const folderMemberIndices: number[] = [];
              for (let position = row.start; position < row.end; position++) {
                const index = displayOrder[position];
                if (isExecutableOperation(effectiveOperation(plan, reversedRows, index))) {
                  folderMemberIndices.push(index);
                }
              }
              onSetRowsIncluded(folderMemberIndices, value);
            };
            return (
              <TreeDepthTableRow
                key={`f:${row.folderPath}`}
                className="folder-group grp"
                treeDepth={row.depth}
                role="row"
                aria-rowindex={logicalRowIndex + 2}
                aria-level={row.depth + 1}
                aria-expanded={!isFolderFolded}
                aria-label={`${isRootFolder ? ROOT_LEVEL_LABEL : row.folderPath}, ${row.count} ${
                  row.count === 1 ? 'item' : 'items'
                }`}
                aria-describedby={synchronizationStatusId}
                data-plan-logical-row={logicalRowIndex}
                tabIndex={isActiveRow ? 0 : -1}
                onFocusCapture={() => activatePlanRow(logicalRowIndex)}
                onKeyDown={(event) => {
                  if (handleRowNavigation(event, logicalRowIndex)) return;
                  if (event.target !== event.currentTarget) return;
                  if (event.key === 'Enter') {
                    event.preventDefault();
                    onToggleFolderFold(row.folderPath);
                  } else if (event.key === ' '
                    && reviewEditable
                    && row.executableCount > 0
                  ) {
                    event.preventDefault();
                    toggleFolderSelection(!allFolderActionsSelected);
                  }
                }}
              >
                <td className="c-synchronize" role="gridcell" aria-colindex={1}>
                  <IndeterminateCheckbox
                    checked={allFolderActionsSelected}
                    indeterminate={someFolderActionsSelected}
                    disabled={!reviewEditable || row.executableCount === 0}
                    ariaLabel={allFolderActionsSelected
                      ? `Remove all executable actions in ${folderLabel} from the Synchronize selection`
                      : `Select all in-scope executable actions in ${folderLabel} for Synchronize`}
                    title={allFolderActionsSelected
                      ? 'Remove this folder’s executable actions from the Synchronize selection'
                      : 'Select this folder’s in-scope executable actions for Synchronize'}
                    tabIndex={isActiveRow ? 0 : -1}
                    onChange={toggleFolderSelection}
                  />
                  <span id={synchronizationStatusId} className="sr-only">
                    {selectedForSyncCount} of {row.executableCount} executable actions selected for Synchronize
                  </span>
                </td>
                <td
                  role="gridcell"
                  aria-colindex={2}
                  aria-colspan={columnCount - 1}
                  colSpan={columnCount - 1}
                  title={`${plan.header.source_root}\n${plan.header.target_root}\n… ${row.folderPath || ROOT_LEVEL_LABEL}`}
                >
                  <button
                    type="button"
                    className="folder-group-toggle"
                    aria-label={isRootFolder
                      ? `${isFolderFolded ? 'Show' : 'Hide'} root-level items`
                      : `${isFolderFolded ? 'Expand' : 'Collapse'} folder ${row.folderPath}`}
                    aria-expanded={!isFolderFolded}
                    tabIndex={isActiveRow ? 0 : -1}
                    onClick={() => onToggleFolderFold(row.folderPath)}
                  >
                    <span className="folder-group-chevron" aria-hidden="true">{isFolderFolded ? <ChevronRight size={13} /> : <ChevronDown size={13} />}</span>
                    <span className="folder-group-icon" aria-hidden="true">{isFolderFolded ? <Folder size={13} /> : <FolderOpen size={13} />}</span>
                    <span className="folder-group-name mono">{folderLabel}</span>
                    <span className="folder-group-summary">{row.count} {row.count === 1 ? 'item' : 'items'}{bytes ? ` · ${humanSize(bytes)}` : ''}</span>
                  </button>
                </td>
              </TreeDepthTableRow>
            );
          }

          const index = row;
          const operation = effectiveOperation(plan, reversedRows, index);
          const groupFolderPath = grouped ? owningFolderOf(operation) : null;
          const folderTreeDepth = groupFolderPath === null || groupFolderPath === ''
            ? 0
            : groupFolderPath.split('/').length - 1;
          const executableOperation = isExecutableOperation(operation);
          const isReversible = canReverseOperation(plan, index);
          const actionPresentation = describeRowAction(operation);
          const [sourcePath, targetPath] = sidePaths(operation);
          const sideMetadata = rowMetadata(plan, index);
          const newerMetadataSide = newerSide(plan, index);
          const highlightedColumn = newerMetadataSide
            ? highlightColumnForSide(newerMetadataSide === 's' ? 'source' : 'target')
            : null;

          // Relative tree rows compact entries owned by their displayed folder to a basename. Full
          // mode is literal even in the tree: choosing it must never continue showing compact paths.
          // When this side's size column has dropped out, the path tooltip absorbs its numbers.
          const buildPathCell = (
            relativePath: string | null,
            rootPath: string,
            sideMetadata: SideMeta | null,
            side: TableSide,
          ): TableCell => {
            if (!relativePath) return { className: 'mono dim' };
            const absolutePath = joinDisplayPath(rootPath, relativePath);
            const isFolderItself = groupFolderPath !== null
              && operation.action === 'delete_dir'
              && relativePath === groupFolderPath;
            const displayPath = pathMode === 'full'
              ? absolutePath
              : isFolderItself
                ? '(this folder)'
                : groupFolderPath !== null && parentRelativePath(relativePath) === groupFolderPath
                  ? relativePathBaseName(relativePath)
                  : relativePath;
            const sideColumns = SIDE_COLUMN_IDS[side];
            const metadataColumnsHidden = !visibleColumnIds.has(sideColumns.size);
            return {
              className: 'mono' + (highlightedColumn === sideColumns.path ? ' newer' : ''),
              title: sideMetadata && metadataColumnsHidden
                ? `${absolutePath}\n${formatMetadataTitle(sideMetadata)}`
                : absolutePath,
              children: displayPath,
            };
          };

          const cellsByColumn: Record<ColumnId, TableCell> = {
            synchronize: {
              children: (
                <>
                  <IndeterminateCheckbox
                    checked={includedRows[index]}
                    disabled={!reviewEditable || !executableOperation}
                    ariaLabel={!executableOperation
                      ? `${operation.path} is informational and cannot be selected for Synchronize`
                      : includedRows[index]
                        ? `Remove ${operation.path} from the Synchronize selection`
                        : `Select ${operation.path} for Synchronize`}
                    title={!executableOperation
                      ? 'Informational result; Synchronize will not run it'
                      : includedRows[index]
                        ? 'Remove this in-scope action from the Synchronize selection'
                        : 'Select this in-scope action for Synchronize'}
                    tabIndex={isActiveRow ? 0 : -1}
                    onChange={(value) => onSetRowIncluded(index, value)}
                  />
                  <span id={synchronizationStatusId} className="sr-only">
                    {executableOperation
                      ? includedRows[index]
                        ? 'Selected for Synchronize'
                        : 'Not selected for Synchronize'
                      : 'Informational result; not executable by Synchronize'}
                  </span>
                </>
              ),
            },
            's.path': buildPathCell(sourcePath, plan.header.source_root, sideMetadata.src, 'source'),
            's.size': buildSizeCell(sideMetadata.src, highlightedColumn === 's.size'),
            's.mtime': buildTimestampCell(sideMetadata.src, highlightedColumn === 's.mtime'),
            action: {
              title: [
                visibleColumnIds.has('reason') ? '' : operation.reason,
                isReversible && reviewEditable
                  ? 'Activate to reverse the direction (activate again to restore)'
                  : '',
              ].filter(Boolean).join('\n') || undefined,
              children: (
                isReversible && reviewEditable ? (
                <button
                  type="button"
                  className={`plan-row-action result-type-${actionPresentation.resultType} reversible`}
                  aria-pressed={reversedRows[index]}
                  aria-label={`${reversedRows[index] ? 'Restore' : 'Reverse'} direction for ${operation.path}: ${actionPresentation.label}`}
                  tabIndex={isActiveRow ? 0 : -1}
                  onClick={() => onToggleRowDirection(index)}
                >
                  {/* Both glyph slots are always rendered at a fixed width: reports have no
                      direction, and without a reserved slot its label would start 16px to the left
                      of every other row's — the arrow is the glyph you scan down this column, so its
                      x has to be the same on every row. */}
                  <span className="plan-row-action-direction" aria-hidden="true">{actionPresentation.direction ? DIRECTION_ICON[actionPresentation.direction] : null}</span>
                  <span className="plan-row-action-result" aria-hidden="true">{RESULT_TYPE_ICON[actionPresentation.resultType]}</span>
                  <span className="plan-row-action-label">{actionPresentation.label}</span>
                </button>
                ) : (
                  <span className={`plan-row-action result-type-${actionPresentation.resultType}`}>
                    <span className="plan-row-action-direction" aria-hidden="true">{actionPresentation.direction ? DIRECTION_ICON[actionPresentation.direction] : null}</span>
                    <span className="plan-row-action-result" aria-hidden="true">{RESULT_TYPE_ICON[actionPresentation.resultType]}</span>
                    <span className="plan-row-action-label">{actionPresentation.label}</span>
                  </span>
                )
              ),
            },
            't.path': buildPathCell(targetPath, plan.header.target_root, sideMetadata.dst, 'target'),
            't.size': buildSizeCell(sideMetadata.dst, highlightedColumn === 't.size'),
            't.mtime': buildTimestampCell(sideMetadata.dst, highlightedColumn === 't.mtime'),
            reason: { children: operation.reason },
          };

          return (
            <TreeDepthTableRow
              key={`r:${index}`}
              className={[
                !includedRows[index] && 'excluded',
                reversedRows[index] && 'reversed',
                groupFolderPath !== null && 'in-folder-group',
                hasAlternatingBackground && 'alternating',
              ]
                .filter(Boolean).join(' ')}
              treeDepth={groupFolderPath === null ? undefined : folderTreeDepth}
              role="row"
              aria-rowindex={logicalRowIndex + 2}
              aria-level={grouped ? folderTreeDepth + 2 : undefined}
              aria-selected={executableOperation ? includedRows[index] : undefined}
              aria-describedby={synchronizationStatusId}
              data-plan-logical-row={logicalRowIndex}
              tabIndex={isActiveRow ? 0 : -1}
              onFocusCapture={() => activatePlanRow(logicalRowIndex)}
              onContextMenu={(event) => {
                event.preventDefault();
                event.currentTarget.focus();
                onContextRow(index, event.clientX, event.clientY);
              }}
              onKeyDown={(event) => {
                if (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) {
                  event.preventDefault();
                  const rowBounds = event.currentTarget.getBoundingClientRect();
                  onContextRow(index, rowBounds.left + 24, rowBounds.top + rowBounds.height / 2);
                  return;
                }
                if (handleRowNavigation(event, logicalRowIndex)) return;
                if (event.target === event.currentTarget
                  && event.key === ' '
                  && reviewEditable
                  && executableOperation
                ) {
                  event.preventDefault();
                  onSetRowIncluded(index, !includedRows[index]);
                }
              }}
            >
              {visibleColumnDefinitions.map((column, columnIndex) => {
                const cell = cellsByColumn[column.id];
                return (
                  <td
                    key={column.id}
                    className={cell.className ? `${column.className} ${cell.className}` : column.className}
                    title={cell.title}
                    role="gridcell"
                    aria-colindex={columnIndex + 1}
                  >
                    {cell.children}
                  </td>
                );
              })}
            </TreeDepthTableRow>
          );
        })}
    </tbody>
  );

  // `useVirtualRows` maps the complete logical list onto a bounded physical canvas. Do not put the
  // logical total or row offset directly into CSS: that recreates a multi-million-pixel render space
  // even though only one screenful of rows is mounted.
  return (
    <div
      ref={tableCanvasRef}
      className="plan-table-canvas"
      role={grouped ? 'treegrid' : 'grid'}
      aria-labelledby={gridLabelId}
      aria-describedby={gridInstructionsId}
      aria-rowcount={rowPlan.length + 1}
      aria-colcount={columnCount}
      aria-multiselectable="true"
    >
      <span id={gridLabelId} className="sr-only">Synchronization result review</span>
      <span id={gridInstructionsId} className="sr-only">
        Sync checkboxes select in-scope executable actions for Synchronize. Use Up and Down Arrow
        to move between rows, Space to change the current row’s Synchronize selection, Tab to reach
        its controls, and Shift F10 to open an operation’s context menu.
      </span>
      <table className="plan-table plan-table-header" role="presentation">
        <PlanTableColumnGroup columns={visibleColumnDefinitions} layout={columnLayout} />
        <thead ref={headerSectionRef} role="rowgroup">
          <tr role="row" aria-rowindex={1}>
            {visibleColumnDefinitions.map((column, columnIndex) => {
              const ownedSortKeys = column.id === 'synchronize'
                ? []
                : [column.id, ...(column.adoptedSortKeys?.[columnLayout] ?? [])];
              const activeSortDirection = activeSort && ownedSortKeys.includes(activeSort.key)
                ? activeSort.dir
                : null;
              const columnTitle = reviewEditable && column.editableTitle !== undefined
                ? column.editableTitle
                : column.title;
              return (
              <th
                key={column.id}
                className={column.className}
                title={columnTitle}
                scope="col"
                role="columnheader"
                aria-colindex={columnIndex + 1}
                aria-sort={activeSortDirection === null
                  ? undefined
                  : activeSortDirection === 1 ? 'ascending' : 'descending'}
              >
                {column.id === 'synchronize' ? (
                  <span className="synchronize-selection-header">
                    <span title="Rows selected for Synchronize">Sync</span>
                    <IndeterminateCheckbox
                      checked={allExecutableRowsSelectedForSync}
                      indeterminate={someExecutableRowsSelectedForSync && !allExecutableRowsSelectedForSync}
                      disabled={!reviewEditable || executableInScope.length === 0}
                      ariaLabel={allExecutableRowsSelectedForSync
                        ? 'Remove all in-scope executable actions from the Synchronize selection'
                        : 'Select all in-scope executable actions for Synchronize'}
                      title={allExecutableRowsSelectedForSync
                        ? 'Remove all in-scope actions from the Synchronize selection'
                        : 'Select all in-scope executable actions for Synchronize'}
                      onChange={(value) => onSetRowsIncluded(executableInScope, value)}
                    />
                  </span>
                ) : (
                  <>
                    <SortHeader sortKey={column.id} sort={activeSort} onSort={onSort} />
                    {(column.adoptedSortKeys?.[columnLayout] ?? []).map((adoptedKey) => (
                      <span key={adoptedKey}> · <SortHeader sortKey={adoptedKey} sort={activeSort} onSort={onSort} /></span>
                    ))}
                  </>
                )}
              </th>
              );
            })}
          </tr>
        </thead>
      </table>
      <table
        ref={bodyTableRef}
        className="plan-table plan-table-body"
        role="presentation"
      >
        <PlanTableColumnGroup columns={visibleColumnDefinitions} layout={columnLayout} />
        {tableBody}
      </table>
    </div>
  );
}
