import { useLayoutEffect, useRef, type ComponentPropsWithoutRef, type ReactNode } from 'react';
import { ChevronDown, ChevronUp } from 'lucide-react';
import { formatCount, formatFileTimestamp, humanSize } from '#core/shared/format.ts';
import { useCssVariables } from '#ui/shared/hooks/useCssVariables.ts';
import type { Sort, SortKey } from '#core/domain/compare/plan.ts';
import type { SideMeta } from '#core/types/generated/SideMeta.ts';
import {
  COLUMN_HEADER_LABELS,
  columnWidthForLayout,
  type ColumnDefinition,
  type ColumnLayout,
} from '../../model/planTableColumns.ts';

export function SortHeader(props: {
  sortKey: SortKey;
  sort: Sort | null;
  onSort: (key: SortKey) => void;
}) {
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
export function IndeterminateCheckbox(props: {
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

export interface TableCell { className?: string; title?: string; children?: ReactNode }

export const formatMetadataTitle = (metadata: SideMeta) => (
  `${formatCount(metadata.size)} bytes\n${new Date(metadata.mtime_ms).toLocaleString()}`
);

const ABSENT_METADATA_CELL: TableCell = { className: 'mono dim', children: '—' };

export function buildSizeCell(metadata: SideMeta | null, highlighted: boolean): TableCell {
  if (!metadata) return ABSENT_METADATA_CELL;
  return {
    className: 'mono' + (highlighted ? ' newer' : ''),
    title: formatMetadataTitle(metadata),
    children: humanSize(metadata.size),
  };
}

export function buildTimestampCell(metadata: SideMeta | null, highlighted: boolean): TableCell {
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
  useCssVariables(
    columnRef,
    { '--plan-table-column-width': widthPixels === null ? null : `${widthPixels}px` },
    [widthPixels],
  );
  return <col ref={columnRef} className="plan-table-column" />;
}

export function PlanTableColumnGroup(props: {
  columns: ColumnDefinition[];
  layout: ColumnLayout;
}) {
  const { columns, layout } = props;
  return (
    <colgroup>
      {columns.map((column) => (
        <PlanTableColumn
          key={column.id}
          widthPixels={columnWidthForLayout(column, layout)}
        />
      ))}
    </colgroup>
  );
}

type TreeDepthTableRowProps = Omit<ComponentPropsWithoutRef<'tr'>, 'style'> & {
  treeDepth?: number;
};

export function TreeDepthTableRow(props: TreeDepthTableRowProps) {
  const { treeDepth, ...rowProps } = props;
  const tableRowRef = useRef<HTMLTableRowElement>(null);
  useCssVariables(
    tableRowRef,
    { '--tree-depth': treeDepth === undefined ? null : String(treeDepth) },
    [treeDepth],
  );
  return <tr ref={tableRowRef} {...rowProps} />;
}
