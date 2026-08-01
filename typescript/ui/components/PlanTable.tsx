import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { ChevronDown, ChevronRight, ChevronUp, Folder, FolderOpen } from 'lucide-react';
import { baseOf, dirOf, fmtTime, fullPath, humanSize } from '../../core/format';
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
import type { CSSProperties, ReactNode } from 'react';
import type { RowSpec } from '../../core/grouping';
import type { PlanDto, SortKey, Sort } from '../../core/plan';
import type { SideMeta } from '../../core/types/generated/SideMeta';

interface PlanTableProps {
  plan: PlanDto;
  flipped: boolean[];
  checked: boolean[];
  rowPlan: RowSpec[];
  // Collapsed descendants remain in this display order so folder ranges and CSV stay complete.
  displayOrder: number[];
  inScopeIndices: number[];
  pathMode: 'rel' | 'full';
  grouped: boolean;
  sort: Sort | null;
  collapsedFolderPaths: Set<string>;
  // Both virtualization and responsive columns measure the caller-owned scroll container.
  wrap: HTMLElement | null;
  onToggleRow: (index: number, value: boolean) => void;
  onToggleMany: (indices: number[], value: boolean) => void;
  onFlip: (index: number) => void;
  onToggleFolderFold: (folderPath: string) => void;
  onSort: (key: SortKey) => void;
  onContextRow: (index: number, x: number, y: number) => void;
}

/// Named for what each rung drops. They are cumulative: `nosize` has no time column either, having
/// already lost it one rung up.
type ColumnMode = 'full' | 'noreason' | 'notime' | 'nosize';

/// A column's identity **is** its sort key — there is no column you can sort by two ways, and no key
/// without a column. Only the checkbox has neither.
type ColumnId = 'chk' | SortKey;

interface ColumnDefinition {
  id: ColumnId;
  /// Keys this header takes over in the modes where their own column is gone. A column that drops
  /// must not take its sort key with it: the header that owns the key would then be unmounted in
  /// exactly the mode where it is the last place left to click, leaving a stale sort you can see but
  /// not change.
  adoptedSortKeys?: Partial<Record<ColumnMode, SortKey[]>>;
  /// Width per mode. **A mode absent from this map means the column is not rendered in it** —
  /// presence and width are one fact, so they cannot drift apart. null = flexible: only the two path
  /// columns, which get no <col> width and so split whatever the fixed ones leave, evenly.
  widths: Partial<Record<ColumnMode, number | null>>;
  /// Class on both <th> and <td>. Never a width — <colgroup> owns those.
  className: string;
  /// Header tooltip, for a column whose behavior needs a sentence the label cannot carry
  title?: string;
}

/// Header text. Short and contextual, because the column it sits over says which side it is;
/// SORT_LABEL in core/plan.ts carries the unambiguous name for the "sorted by" indicator.
const COLUMN_HEADER: Record<SortKey, string> = {
  's.path': 'source', 't.path': 'target', action: 'action',
  's.size': 'size', 't.size': 'size', 's.mtime': 'time', 't.mtime': 'time',
  reason: 'reason',
};

/// The table's whole layout, in order. The row reads left to right as source facts → what happens →
/// target facts, so the action sits on the axis between the two sides rather than in front of them,
/// and each side's size and time are their own columns rather than one fused cell.
///
/// One descriptor per column drives the <colgroup>, the <thead> and the row cells. The alternative —
/// a width array per mode plus `mode === 'full' &&` guards repeated in the header and the body — is a
/// dozen places that have to agree, and under `table-layout: fixed` a disagreement is *silent*:
/// surplus <col>s are ignored and missing ones make the trailing columns split the remainder, so the
/// failure reads as "the widths went odd at one window size" and nobody bisects it.
///
/// Widths at each mode's own floor leave the two path columns 191 / 191 / 157 / 199 px — they are
/// the primary content and never get starved. The 1240→1000 step is exactly the reason column's
/// 240px, so crossing it costs the paths nothing.
const COLUMNS: ColumnDefinition[] = [
  { id: 'chk', className: 'c-chk', widths: { full: 38, noreason: 38, notime: 38, nosize: 38 } },
  {
    id: 's.path', className: 'c-path c-path-s',
    adoptedSortKeys: { nosize: ['s.size', 's.mtime'] },
    widths: { full: null, noreason: null, notime: null, nosize: null },
  },
  // 112 rather than 92 in notime: an ellipsized "size · ti…" would leave the time span unclickable,
  // and thead clips. 92 fits "894.0 MB"; 112 fits the composite header above it.
  { id: 's.size', className: 'c-size', adoptedSortKeys: { notime: ['s.mtime'] }, widths: { full: 92, noreason: 92, notime: 112 } },
  { id: 's.mtime', className: 'c-time', widths: { full: 136, noreason: 136 } },
  {
    id: 'action', className: 'c-act',
    widths: { full: 124, noreason: 124, notime: 124, nosize: 124 },
    title: "Sort by action. Activate a row's action to reverse its direction; activate it again to restore.",
  },
  {
    id: 't.path', className: 'c-path c-path-t',
    adoptedSortKeys: { nosize: ['t.size', 't.mtime'] },
    widths: { full: null, noreason: null, notime: null, nosize: null },
  },
  { id: 't.size', className: 'c-size', adoptedSortKeys: { notime: ['t.mtime'] }, widths: { full: 92, noreason: 92, notime: 112 } },
  { id: 't.mtime', className: 'c-time', widths: { full: 136, noreason: 136 } },
  { id: 'reason', className: 'c-reason', widths: { full: 240 } },
];

const columnModeForWidth = (width: number): ColumnMode => (
  width >= 1240 ? 'full' : width >= 1000 ? 'noreason' : width >= 700 ? 'notime' : 'nosize'
);

/// The narrowest a path column may be before the table stops shrinking and scrolls sideways instead.
/// Under fixed layout the columns with no <col> width absorb every shortfall, so without a floor they
/// go to zero and the paths vanish entirely rather than the table admitting it has run out of room.
const MINIMUM_PATH_WIDTH = 140;

/// Minimum table width for a column set: everything pinned, plus a floor for each path column. A
/// static number cannot do this job — the pinned total is 858 in `full` and 162 in `nosize`, so one
/// value is either far too wide for the narrow set or no constraint at all for the wide one.
function minimumTableWidth(columns: ColumnDefinition[], mode: ColumnMode): number {
  let fixedWidth = 0;
  let flexibleColumnCount = 0;
  for (const column of columns) {
    const width = column.widths[mode];
    if (width == null) flexibleColumnCount++; else fixedWidth += width;
  }
  return fixedWidth + flexibleColumnCount * MINIMUM_PATH_WIDTH;
}

/// The column set tracks the scroll container because collapsing Run Scope changes that width
/// without resizing the window.
function useContainerWidth(container: HTMLElement | null): number {
  const [width, setWidth] = useState(1600);
  useLayoutEffect(() => {
    if (!container) return;
    const observer = new ResizeObserver(() => setWidth(container.clientWidth));
    observer.observe(container);
    setWidth(container.clientWidth);
    return () => observer.disconnect();
  }, [container]);
  return width;
}

/// A clickable header. Declared at module scope, not inside PlanTable: a component defined in a
/// render body is a *new type* on every render, so React unmounts and rebuilds every one of these
/// controls — and this table re-renders on every scroll frame.
function SortHeader(props: { sortKey: SortKey; sort: Sort | null; onSort: (key: SortKey) => void }) {
  const { sortKey, sort, onSort } = props;
  const active = sort?.key === sortKey;
  const state = active ? `, currently ${sort.dir === 1 ? 'ascending' : 'descending'}` : '';
  return (
    <button
      type="button"
      className={'sortable' + (active ? ' on' : '')}
      aria-pressed={active}
      aria-label={`Sort by ${COLUMN_HEADER[sortKey]}${state}`}
      onClick={() => onSort(sortKey)}
    >
      {COLUMN_HEADER[sortKey]}
      {active && (
        <span className="sortmark">
          {sort.dir === 1 ? <ChevronUp size={11} /> : <ChevronDown size={11} />}
        </span>
      )}
    </button>
  );
}

/// indeterminate is a DOM property with no HTML attribute, so it can only be set through a ref
function TriCheckbox(props: { checked: boolean; indeterminate?: boolean; disabled?: boolean; title?: string; ariaLabel: string; onChange: (value: boolean) => void; stopClick?: boolean }) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => { if (ref.current) ref.current.indeterminate = !!props.indeterminate; });
  return (
    <input
      ref={ref}
      type="checkbox"
      checked={props.checked}
      disabled={props.disabled}
      title={props.title}
      aria-label={props.ariaLabel}
      onClick={props.stopClick ? (event) => event.stopPropagation() : undefined}
      onChange={(event) => props.onChange(event.target.checked)}
    />
  );
}

/// What a column contributes to one row. The <td> itself is emitted by the single loop below, so
/// the cell count can never disagree with the <col> count.
interface TableCell { className?: string; title?: string; children?: ReactNode }

/// Both meta cells carry the whole truth in their tooltip regardless of mode, so the contract does
/// not change with the window width — that is what lets the time column drop without losing it.
const metadataTitle = (metadata: SideMeta) => (
  `${metadata.size.toLocaleString()} bytes\n${new Date(metadata.mtime_ms).toLocaleString()}`
);
const ABSENT_CELL: TableCell = { className: 'mono dim', children: '—' };

function sizeCell(metadata: SideMeta | null, highlighted: boolean): TableCell {
  if (!metadata) return ABSENT_CELL;
  return {
    className: 'mono' + (highlighted ? ' newer' : ''),
    title: metadataTitle(metadata),
    children: humanSize(metadata.size),
  };
}

function timeCell(metadata: SideMeta | null, highlighted: boolean): TableCell {
  if (!metadata) return ABSENT_CELL;
  return {
    className: 'mono' + (highlighted ? ' newer' : ''),
    title: metadataTitle(metadata),
    children: fmtTime(metadata.mtime_ms),
  };
}

export function PlanTable(props: PlanTableProps) {
  const {
    plan, flipped, checked, rowPlan, displayOrder, inScopeIndices, pathMode, grouped, sort, collapsedFolderPaths, wrap,
    onToggleRow, onToggleMany, onFlip, onToggleFolderFold, onSort, onContextRow,
  } = props;

  const theadRef = useRef<HTMLTableSectionElement>(null);
  const bodyRef = useRef<HTMLTableSectionElement>(null);
  const gridLabelId = useId();

  useEffect(() => { if (wrap) wrap.scrollTop = 0; }, [grouped, inScopeIndices, sort, wrap]);

  const virtualWindow = useVirtualRows(rowPlan, wrap, theadRef, bodyRef);
  const mode = columnModeForWidth(useContainerWidth(wrap));
  const columns = COLUMNS.filter((column) => column.widths[mode] !== undefined);
  const visibleColumns = new Set<ColumnId>(columns.map((column) => column.id));
  const columnCount = columns.length;
  const tableMinWidth = minimumTableWidth(columns, mode);

  const colGroup = () => (
    <colgroup>
      {columns.map((column) => {
        const width = column.widths[mode];
        return <col key={column.id} style={width != null ? { width } : undefined} />;
      })}
    </colgroup>
  );

  // Scrolling updates this component's local virtual-window state every animation frame. These two
  // full-plan passes used to run on every one of those renders; at several hundred thousand rows,
  // trackpad momentum allocated another multi-megabyte index array per frame until WebKit hit
  // memory pressure and painted the window black.
  const executableInScope = useMemo(
    () => inScopeIndices.filter((index) => isExecutableOperation(effectiveOperation(plan, flipped, index))),
    [plan, flipped, inScopeIndices],
  );
  const allChecked = useMemo(
    () => executableInScope.length > 0 && executableInScope.every((index) => checked[index]),
    [executableInScope, checked],
  );
  const someChecked = useMemo(
    () => executableInScope.some((index) => checked[index]),
    [executableInScope, checked],
  );

  // A folder row needs the checked count of an arbitrary subtree. Prefixing the one DFS order once
  // makes that O(1) per rendered folder, instead of repeatedly scanning a 100k-file parent on every
  // virtual-scroll render. Descendant indices themselves are materialized only when a checkbox is
  // actually clicked.
  const checkedPrefix = useMemo(() => {
    const prefix = new Uint32Array(displayOrder.length + 1);
    for (let position = 0; position < displayOrder.length; position++) {
      const index = displayOrder[position];
      prefix[position + 1] = prefix[position]
        + (checked[index] && isExecutableOperation(effectiveOperation(plan, flipped, index)) ? 1 : 0);
    }
    return prefix;
  }, [plan, flipped, checked, displayOrder]);

  /// Where "this side is newer" gets painted: the claim is about the mtime, so it takes the most
  /// specific of that side's columns still on screen, falling back to the path once the narrowest
  /// rung has dropped both meta columns. Without that fallback the single most decision-critical
  /// hint in the table would simply vanish below 700px.
  const newerCol = (side: 's' | 't'): ColumnId =>
    (visibleColumns.has(`${side}.mtime`)
      ? `${side}.mtime`
      : visibleColumns.has(`${side}.size`)
        ? `${side}.size`
        : `${side}.path`);

  const rows = (
    <tbody ref={bodyRef} role="rowgroup">
        {rowPlan.slice(virtualWindow.from, virtualWindow.to).map((row, visibleOffset) => {
          // Zebra striping keys off the real row index, not :nth-child — otherwise the stripes flip as
          // the window scrolls
          const alternate = (virtualWindow.from + visibleOffset) % 2 === 1;

          if (typeof row !== 'number') {
            const { bytes } = row;
            const checkedCount = checkedPrefix[row.end] - checkedPrefix[row.start];
            const folded = collapsedFolderPaths.has(row.folderPath);
            const isRoot = row.folderPath === ROOT_FOLDER_PATH;
            const label = isRoot ? ROOT_LEVEL_LABEL : baseOf(row.folderPath);
            const treeStyle = { '--tree-depth': row.depth } as CSSProperties;
            const toggleFolder = (value: boolean) => {
              const memberIndices: number[] = [];
              for (let position = row.start; position < row.end; position++) {
                const index = displayOrder[position];
                if (isExecutableOperation(effectiveOperation(plan, flipped, index))) {
                  memberIndices.push(index);
                }
              }
              onToggleMany(memberIndices, value);
            };
            return (
              <tr
                key={`f:${row.folderPath}`}
                className="grp"
                style={treeStyle}
                role="row"
                aria-rowindex={virtualWindow.from + visibleOffset + 2}
              >
                <td className="c-chk" role="gridcell" aria-colindex={1}>
                  <TriCheckbox
                    checked={row.executableCount > 0 && checkedCount === row.executableCount}
                    indeterminate={checkedCount > 0 && checkedCount < row.executableCount}
                    disabled={row.executableCount === 0}
                    ariaLabel={isRoot
                      ? 'Select in-scope root-level items'
                      : `Select in-scope items in folder ${row.folderPath}`}
                    title={isRoot
                      ? 'Check / uncheck in-scope root-level items'
                      : 'Check / uncheck in-scope items in this folder and its subfolders'}
                    onChange={toggleFolder}
                  />
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
                    className="gtree"
                    aria-label={isRoot
                      ? `${folded ? 'Show' : 'Hide'} root-level items`
                      : `${folded ? 'Expand' : 'Collapse'} folder ${row.folderPath}`}
                    aria-expanded={!folded}
                    onClick={() => onToggleFolderFold(row.folderPath)}
                  >
                    <span className="gchev">{folded ? <ChevronRight size={13} /> : <ChevronDown size={13} />}</span>
                    <span className="gfolder">{folded ? <Folder size={13} /> : <FolderOpen size={13} />}</span>
                    <span className="gdir mono">{label}</span>
                    <span className="gmeta">{row.count} {row.count === 1 ? 'item' : 'items'}{bytes ? ` · ${humanSize(bytes)}` : ''}</span>
                  </button>
                </td>
              </tr>
            );
          }

          const index = row;
          const operation = effectiveOperation(plan, flipped, index);
          const groupFolderPath = grouped ? owningFolderOf(operation) : null;
          const treeDepth = groupFolderPath === null || groupFolderPath === ''
            ? 0
            : groupFolderPath.split('/').length - 1;
          const reversible = canReverseOperation(plan, index);
          const actionPresentation = describeRowAction(operation);
          const [sourcePath, targetPath] = sidePaths(operation);
          const metadata = rowMetadata(plan, index);
          const newerMetadataSide = newerSide(plan, index);
          const highlightedColumn = newerMetadataSide ? newerCol(newerMetadataSide) : null;

          // Relative tree rows compact entries owned by their displayed folder to a basename. Full
          // mode is literal even in the tree: choosing it must never continue showing compact paths.
          // When this side's size column has dropped out, the path tooltip absorbs its numbers.
          const pathCell = (
            relativePath: string | null,
            rootPath: string,
            sideMetadata: SideMeta | null,
            side: 's' | 't',
          ): TableCell => {
            if (!relativePath) return { className: 'mono dim' };
            const absolutePath = fullPath(rootPath, relativePath);
            const folderSelf = groupFolderPath !== null
              && operation.action === 'delete_dir'
              && relativePath === groupFolderPath;
            const text = pathMode === 'full'
              ? absolutePath
              : folderSelf
                ? '(this folder)'
                : groupFolderPath !== null && dirOf(relativePath) === groupFolderPath
                  ? baseOf(relativePath)
                  : relativePath;
            const metadataColumnsHidden = !visibleColumns.has(`${side}.size`);
            return {
              className: 'mono' + (highlightedColumn === `${side}.path` ? ' newer' : ''),
              title: sideMetadata && metadataColumnsHidden
                ? `${absolutePath}\n${metadataTitle(sideMetadata)}`
                : absolutePath,
              children: text,
            };
          };

          // Built for every column, rendered for the ones this mode shows — here rather than in a
          // per-column render function because every value it needs is already a local.
          const cells: Record<ColumnId, TableCell> = {
            chk: {
              children: (
                <TriCheckbox
                  checked={checked[index]}
                  disabled={!isExecutableOperation(operation)}
                  ariaLabel={`${checked[index] ? 'Exclude' : 'Include'} ${operation.path} in Run Scope`}
                  onChange={(value) => onToggleRow(index, value)}
                />
              ),
            },
            's.path': pathCell(sourcePath, plan.header.source_root, metadata.src, 's'),
            's.size': sizeCell(metadata.src, highlightedColumn === 's.size'),
            's.mtime': timeCell(metadata.src, highlightedColumn === 's.mtime'),
            action: {
              // With the reason column folded away, the action cell's tooltip carries the reason
              title: [
                visibleColumns.has('reason') ? '' : operation.reason,
                reversible ? 'Activate to reverse the direction (activate again to restore)' : '',
              ].filter(Boolean).join('\n') || undefined,
              children: (
                reversible ? (
                <button
                  type="button"
                  className={`act result-type-${actionPresentation.resultType} flippable`}
                  aria-pressed={flipped[index]}
                  aria-label={`${flipped[index] ? 'Restore' : 'Reverse'} direction for ${operation.path}: ${actionPresentation.label}`}
                  onClick={() => onFlip(index)}
                >
                  {/* Both glyph slots are always rendered at a fixed width: reports have no
                      direction, and without a reserved slot its label would start 16px to the left
                      of every other row's — the arrow is the glyph you scan down this column, so its
                      x has to be the same on every row. */}
                  <span className="act-dir" aria-hidden="true">{actionPresentation.direction ? DIRECTION_ICON[actionPresentation.direction] : null}</span>
                  <span className="act-mark" aria-hidden="true">{RESULT_TYPE_ICON[actionPresentation.resultType]}</span>
                  <span className="act-label">{actionPresentation.label}</span>
                </button>
                ) : (
                  <span className={`act result-type-${actionPresentation.resultType}`}>
                    <span className="act-dir" aria-hidden="true">{actionPresentation.direction ? DIRECTION_ICON[actionPresentation.direction] : null}</span>
                    <span className="act-mark" aria-hidden="true">{RESULT_TYPE_ICON[actionPresentation.resultType]}</span>
                    <span className="act-label">{actionPresentation.label}</span>
                  </span>
                )
              ),
            },
            't.path': pathCell(targetPath, plan.header.target_root, metadata.dst, 't'),
            't.size': sizeCell(metadata.dst, highlightedColumn === 't.size'),
            't.mtime': timeCell(metadata.dst, highlightedColumn === 't.mtime'),
            reason: { children: operation.reason },
          };

          return (
            <tr
              key={`r:${index}`}
              className={[
                !checked[index] && 'off',
                flipped[index] && 'flip',
                groupFolderPath !== null && 'ingrp',
                alternate && 'alt',
              ]
                .filter(Boolean).join(' ')}
              style={groupFolderPath === null ? undefined : ({ '--tree-depth': treeDepth } as CSSProperties)}
              role="row"
              aria-rowindex={virtualWindow.from + visibleOffset + 2}
              aria-selected={checked[index]}
              tabIndex={0}
              onContextMenu={(e) => {
                e.preventDefault();
                e.currentTarget.focus();
                onContextRow(index, e.clientX, e.clientY);
              }}
              onKeyDown={(e) => {
                if (e.key !== 'ContextMenu' && !(e.shiftKey && e.key === 'F10')) return;
                e.preventDefault();
                const rect = e.currentTarget.getBoundingClientRect();
                onContextRow(index, rect.left + 24, rect.top + rect.height / 2);
              }}
            >
              {columns.map((column, columnIndex) => {
                const cell = cells[column.id];
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
            </tr>
          );
        })}
    </tbody>
  );

  // `useVirtualRows` maps the complete logical list onto a bounded physical canvas. Do not put the
  // logical total or row offset directly into CSS: that recreates a multi-million-pixel render space
  // even though only one screenful of rows is mounted.
  return (
    <div
      className="vtable-canvas"
      role="grid"
      aria-labelledby={gridLabelId}
      aria-rowcount={rowPlan.length + 1}
      aria-colcount={columnCount}
      aria-multiselectable="true"
      style={{ minWidth: tableMinWidth, height: virtualWindow.canvasHeight }}
    >
      <span id={gridLabelId} className="sr-only">Synchronization plan</span>
      <table className="plantable vtable-head" role="presentation">
        {colGroup()}
        <thead ref={theadRef} role="rowgroup">
          <tr role="row" aria-rowindex={1}>
            {columns.map((column, columnIndex) => {
              const ownedKeys = column.id === 'chk'
                ? []
                : [column.id, ...(column.adoptedSortKeys?.[mode] ?? [])];
              const ownsSort = !!sort && ownedKeys.includes(sort.key);
              return (
              <th
                key={column.id}
                className={column.className}
                title={column.title}
                scope="col"
                role="columnheader"
                aria-colindex={columnIndex + 1}
                aria-sort={ownsSort ? (sort!.dir === 1 ? 'ascending' : 'descending') : undefined}
              >
                {column.id === 'chk' ? (
                  <TriCheckbox
                    checked={allChecked}
                    indeterminate={someChecked && !allChecked}
                    disabled={executableInScope.length === 0}
                    ariaLabel="Select all executable rows in the current run scope"
                    title="Select all / none in the current run scope"
                    onChange={(value) => onToggleMany(executableInScope, value)}
                  />
                ) : (
                  <>
                    <SortHeader sortKey={column.id} sort={sort} onSort={onSort} />
                    {(column.adoptedSortKeys?.[mode] ?? []).map((adoptedKey) => (
                      <span key={adoptedKey}> · <SortHeader sortKey={adoptedKey} sort={sort} onSort={onSort} /></span>
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
        className="plantable vtable-body"
        role="presentation"
        style={{ transform: `translate3d(0, ${virtualWindow.bodyTop}px, 0)` }}
      >
        {colGroup()}
        {rows}
      </table>
    </div>
  );
}
