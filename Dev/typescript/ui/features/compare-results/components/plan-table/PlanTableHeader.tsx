import type { RefObject } from 'react';
import type { Sort, SortKey } from '#core/domain/compare/plan.ts';
import type { ColumnDefinition, ColumnLayout } from '../../model/planTableColumns.ts';
import { IndeterminateCheckbox, PlanTableColumnGroup, SortHeader } from './PlanTablePrimitives.tsx';

export function PlanTableHeader(props: {
  headerSectionRef: RefObject<HTMLTableSectionElement | null>;
  columns: ColumnDefinition[];
  columnLayout: ColumnLayout;
  activeSort: Sort | null;
  reviewEditable: boolean;
  executableInScope: number[];
  allExecutableRowsSelectedForSync: boolean;
  someExecutableRowsSelectedForSync: boolean;
  onSetRowsIncluded: (indices: number[], value: boolean) => void;
  onSort: (key: SortKey) => void;
}) {
  const {
    headerSectionRef,
    columns,
    columnLayout,
    activeSort,
    reviewEditable,
    executableInScope,
    allExecutableRowsSelectedForSync,
    someExecutableRowsSelectedForSync,
    onSetRowsIncluded,
    onSort,
  } = props;
  return (
    <table className="plan-table plan-table-header" role="presentation">
      <PlanTableColumnGroup columns={columns} layout={columnLayout} />
      <thead ref={headerSectionRef} role="rowgroup">
        <tr role="row" aria-rowindex={1}>
          {columns.map((column, columnIndex) => {
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
                      <span key={adoptedKey}>
                        {' · '}
                        <SortHeader sortKey={adoptedKey} sort={activeSort} onSort={onSort} />
                      </span>
                    ))}
                  </>
                )}
              </th>
            );
          })}
        </tr>
      </thead>
    </table>
  );
}
