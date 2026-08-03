import {
  joinDisplayPath,
  parentRelativePath,
  relativePathBaseName,
} from '#core/shared/format.ts';
import { owningFolderOf } from '#core/domain/compare/folders.ts';
import {
  canReverseOperation,
  describeRowAction,
  effectiveOperation,
  isExecutableOperation,
  newerSide,
  rowMetadata,
  sidePaths,
} from '#core/domain/compare/plan.ts';
import { DIRECTION_ICON, RESULT_TYPE_ICON } from '#ui/shared/icons/compareIcons.tsx';
import {
  IndeterminateCheckbox,
  TreeDepthTableRow,
  buildSizeCell,
  buildTimestampCell,
  formatMetadataTitle,
  type TableCell,
} from './PlanTablePrimitives.tsx';
import { SIDE_COLUMN_IDS, type ColumnDefinition, type ColumnId, type TableSide } from '../../model/planTableColumns.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';
import type { SideMeta } from '#core/types/generated/SideMeta.ts';
import type { PlanTableRowNavigationHandler } from '../../model/planTableNavigation.ts';

interface PlanTableOperationRowProps {
  index: number;
  logicalRowIndex: number;
  isActiveRow: boolean;
  synchronizationStatusId: string;
  hasAlternatingBackground: boolean;
  plan: PlanDto;
  reversedRows: boolean[];
  includedRows: boolean[];
  pathMode: 'relative' | 'full';
  grouped: boolean;
  reviewEditable: boolean;
  visibleColumnDefinitions: ColumnDefinition[];
  visibleColumnIds: Set<ColumnId>;
  onSetRowIncluded: (index: number, value: boolean) => void;
  onToggleRowDirection: (index: number) => void;
  onContextRow: (index: number, x: number, y: number) => void;
  onActivateRow: (logicalRowIndex: number) => void;
  onNavigateRow: PlanTableRowNavigationHandler;
}

export function PlanTableOperationRow(props: PlanTableOperationRowProps) {
  const {
    index,
    logicalRowIndex,
    isActiveRow,
    synchronizationStatusId,
    hasAlternatingBackground,
    plan,
    reversedRows,
    includedRows,
    pathMode,
    grouped,
    reviewEditable,
    visibleColumnDefinitions,
    visibleColumnIds,
    onSetRowIncluded,
    onToggleRowDirection,
    onContextRow,
    onActivateRow,
    onNavigateRow,
  } = props;
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

  // Move the timestamp cue to a visible metadata column in compact layouts.
  const highlightColumnForSide = (side: TableSide): ColumnId => {
    const sideColumns = SIDE_COLUMN_IDS[side];
    if (visibleColumnIds.has(sideColumns.timestamp)) return sideColumns.timestamp;
    if (visibleColumnIds.has(sideColumns.size)) return sideColumns.size;
    return sideColumns.path;
  };
  const highlightedColumn = newerMetadataSide
    ? highlightColumnForSide(newerMetadataSide === 's' ? 'source' : 'target')
    : null;

  // Full mode stays literal in grouped rows; hidden metadata moves into the path tooltip.
  const buildPathCell = (
    relativePath: string | null,
    rootPath: string,
    metadata: SideMeta | null,
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
      title: metadata && metadataColumnsHidden
        ? `${absolutePath}\n${formatMetadataTitle(metadata)}`
        : absolutePath,
      children: displayPath,
    };
  };

  // Fixed glyph slots keep action labels horizontally aligned.
  const actionGlyphs = (
    <>
      <span className="plan-row-action-direction" aria-hidden="true">{actionPresentation.direction ? DIRECTION_ICON[actionPresentation.direction] : null}</span>
      <span className="plan-row-action-result" aria-hidden="true">{RESULT_TYPE_ICON[actionPresentation.resultType]}</span>
      <span className="plan-row-action-label">{actionPresentation.label}</span>
    </>
  );

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
          {actionGlyphs}
        </button>
        ) : (
          <span className={`plan-row-action result-type-${actionPresentation.resultType}`}>
            {actionGlyphs}
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
      onFocusCapture={() => onActivateRow(logicalRowIndex)}
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
        if (onNavigateRow(event, logicalRowIndex)) return;
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
}
