import type { SortKey } from '#core/domain/compare/plan.ts';

export type ColumnLayout =
  | 'allColumns'
  | 'withoutReason'
  | 'withoutReasonOrTimestamps'
  | 'synchronizePathsAndAction';

/// Sortable columns use their sort key as identity; Synchronize is the sole exception.
export type ColumnId = 'synchronize' | SortKey;
export type TableSide = 'source' | 'target';

export const SIDE_COLUMN_IDS = {
  source: { path: 's.path', size: 's.size', timestamp: 's.mtime' },
  target: { path: 't.path', size: 't.size', timestamp: 't.mtime' },
} as const satisfies Record<TableSide, Record<'path' | 'size' | 'timestamp', SortKey>>;

export interface ColumnDefinition {
  id: ColumnId;
  /// A visible header adopts sort keys whose columns are hidden in this layout.
  adoptedSortKeys?: Partial<Record<ColumnLayout, SortKey[]>>;
  /// An absent layout omits the column; null leaves its width flexible.
  widthByLayout: Partial<Record<ColumnLayout, number | null>>;
  className: string;
  title?: string;
  editableTitle?: string;
}

export const COLUMN_HEADER_LABELS: Record<SortKey, string> = {
  's.path': 'source',
  't.path': 'target',
  action: 'action',
  's.size': 'size',
  't.size': 'size',
  's.mtime': 'time',
  't.mtime': 'time',
  reason: 'reason',
};

/// Ordered source of truth for colgroup, headers, cells, and responsive widths. Layout steps
/// remove fixed columns without shrinking the flexible path columns.
export const COLUMN_DEFINITIONS: ColumnDefinition[] = [
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
    id: 's.path',
    className: 'c-path c-source-path',
    adoptedSortKeys: { synchronizePathsAndAction: ['s.size', 's.mtime'] },
    widthByLayout: {
      allColumns: null,
      withoutReason: null,
      withoutReasonOrTimestamps: null,
      synchronizePathsAndAction: null,
    },
  },
  // The composite size/time header needs 112 px to keep both sort controls clickable.
  {
    id: 's.size',
    className: 'c-size',
    adoptedSortKeys: { withoutReasonOrTimestamps: ['s.mtime'] },
    widthByLayout: { allColumns: 92, withoutReason: 92, withoutReasonOrTimestamps: 112 },
  },
  { id: 's.mtime', className: 'c-time', widthByLayout: { allColumns: 136, withoutReason: 136 } },
  {
    id: 'action',
    className: 'c-action',
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
    id: 't.path',
    className: 'c-path c-target-path',
    adoptedSortKeys: { synchronizePathsAndAction: ['t.size', 't.mtime'] },
    widthByLayout: {
      allColumns: null,
      withoutReason: null,
      withoutReasonOrTimestamps: null,
      synchronizePathsAndAction: null,
    },
  },
  {
    id: 't.size',
    className: 'c-size',
    adoptedSortKeys: { withoutReasonOrTimestamps: ['t.mtime'] },
    widthByLayout: { allColumns: 92, withoutReason: 92, withoutReasonOrTimestamps: 112 },
  },
  { id: 't.mtime', className: 'c-time', widthByLayout: { allColumns: 136, withoutReason: 136 } },
  { id: 'reason', className: 'c-reason', widthByLayout: { allColumns: 240 } },
];

export function columnLayoutForWidth(containerWidthPixels: number): ColumnLayout {
  return containerWidthPixels >= 1240
    ? 'allColumns'
    : containerWidthPixels >= 1000
      ? 'withoutReason'
      : containerWidthPixels >= 700
        ? 'withoutReasonOrTimestamps'
        : 'synchronizePathsAndAction';
}

export function columnWidthForLayout(
  column: ColumnDefinition,
  layout: ColumnLayout,
): number | null {
  const width = column.widthByLayout[layout];
  if (width === undefined) {
    throw new Error(`Column ${column.id} is not available in layout ${layout}`);
  }
  return width;
}

/// Below this path width, the fixed-layout table overflows horizontally instead of hiding paths.
const MINIMUM_PATH_COLUMN_WIDTH = 140;

/// Minimum width is derived per layout because each layout has a different fixed-column total.
export function calculateMinimumTableWidth(
  columns: ColumnDefinition[],
  layout: ColumnLayout,
): number {
  let fixedWidth = 0;
  let flexibleColumnCount = 0;
  for (const column of columns) {
    const width = columnWidthForLayout(column, layout);
    if (width === null) flexibleColumnCount++; else fixedWidth += width;
  }
  return fixedWidth + flexibleColumnCount * MINIMUM_PATH_COLUMN_WIDTH;
}
