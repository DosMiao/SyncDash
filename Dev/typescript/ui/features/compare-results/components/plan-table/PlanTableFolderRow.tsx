import { ChevronDown, ChevronRight, Folder, FolderOpen } from 'lucide-react';
import { humanSize, relativePathBaseName } from '#core/shared/format.ts';
import { ROOT_FOLDER_PATH, ROOT_LEVEL_LABEL } from '#core/domain/compare/folders.ts';
import { effectiveOperation, isExecutableOperation } from '#core/domain/compare/plan.ts';
import { IndeterminateCheckbox, TreeDepthTableRow } from './PlanTablePrimitives.tsx';
import type { RowSpec } from '#core/domain/compare/grouping.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';
import type { PlanTableRowNavigationHandler } from '../../model/planTableNavigation.ts';

type FolderRowSpec = Exclude<RowSpec, number>;

interface PlanTableFolderRowProps {
  row: FolderRowSpec;
  logicalRowIndex: number;
  isActiveRow: boolean;
  synchronizationStatusId: string;
  columnCount: number;
  plan: PlanDto;
  reversedRows: boolean[];
  displayOrder: number[];
  reviewEditable: boolean;
  collapsedFolderPaths: Set<string>;
  synchronizationSelectionCountPrefix: Uint32Array;
  onSetRowsIncluded: (indices: number[], value: boolean) => void;
  onToggleFolderFold: (folderPath: string) => void;
  onActivateRow: (logicalRowIndex: number) => void;
  onNavigateRow: PlanTableRowNavigationHandler;
}

export function PlanTableFolderRow(props: PlanTableFolderRowProps) {
  const {
    row,
    logicalRowIndex,
    isActiveRow,
    synchronizationStatusId,
    columnCount,
    plan,
    reversedRows,
    displayOrder,
    reviewEditable,
    collapsedFolderPaths,
    synchronizationSelectionCountPrefix,
    onSetRowsIncluded,
    onToggleFolderFold,
    onActivateRow,
    onNavigateRow,
  } = props;
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
      onFocusCapture={() => onActivateRow(logicalRowIndex)}
      onKeyDown={(event) => {
        if (onNavigateRow(event, logicalRowIndex)) return;
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
