import { useCallback } from 'react';
import type { Dispatch, SetStateAction } from 'react';
import * as ipc from '#core/infrastructure/tauri/commands/main.ts';
import type { CompareWorkspace } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import { owningFolderOf } from '#core/domain/compare/folders.ts';
import {
  canReverseOperation,
  effectiveOperation,
  isExecutableOperation,
  keySpec,
  sidePaths,
} from '#core/domain/compare/plan.ts';
import type { PlanDto, Sort, SortKey } from '#core/domain/compare/plan.ts';
import { matchesFolderScope } from '#core/domain/compare/runScope.ts';
import { joinDisplayPath, relativePathBaseName } from '#core/shared/format.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import type { ContextMenuState } from '../../model/workspacePageModel.ts';

interface ResultTableActionsOptions {
  workspace: CompareWorkspace | null;
  plan: PlanDto | null;
  reversedRows: boolean[];
  inScopeIndices: readonly number[];
  selectedJob: JobDto | null;
  reviewEditable: boolean;
  sort: Sort | null;
  dispatch: Dispatch<CompareWorkspaceAction>;
  setContextMenu: Dispatch<SetStateAction<ContextMenuState | null>>;
  addExcludes: (masks: string[], label: string) => Promise<void>;
  setStatus: StatusApi['setMessage'];
}

export function useResultTableActions({
  workspace,
  plan,
  reversedRows,
  inScopeIndices,
  selectedJob,
  reviewEditable,
  sort,
  dispatch,
  setContextMenu,
  addExcludes,
  setStatus,
}: ResultTableActionsOptions) {
  const replaceIncludedRows = useCallback((next: boolean[] | ((previous: boolean[]) => boolean[])) => {
    if (!workspace || !reviewEditable) return;
    dispatch({
      type: 'row_inclusion_replaced',
      resultKey: workspace.key,
      rowIncluded: typeof next === 'function' ? next(workspace.differences.rowIncluded) : next,
    });
  }, [dispatch, reviewEditable, workspace]);

  const replaceReversedRows = useCallback((next: boolean[] | ((previous: boolean[]) => boolean[])) => {
    if (!workspace || !reviewEditable) return;
    dispatch({
      type: 'row_reversal_replaced',
      resultKey: workspace.key,
      rowReversed: typeof next === 'function' ? next(workspace.differences.rowReversed) : next,
    });
  }, [dispatch, reviewEditable, workspace]);

  const setRowIncluded = useCallback((index: number, value: boolean) => {
    replaceIncludedRows((previous) => {
      const next = [...previous];
      next[index] = value;
      return next;
    });
  }, [replaceIncludedRows]);

  const setRowsIncluded = useCallback((indices: number[], value: boolean) => {
    replaceIncludedRows((previous) => {
      const next = [...previous];
      for (const index of indices) next[index] = value;
      return next;
    });
  }, [replaceIncludedRows]);

  const toggleRowDirection = useCallback((index: number) => {
    replaceReversedRows((previous) => {
      const next = [...previous];
      next[index] = !next[index];
      return next;
    });
  }, [replaceReversedRows]);

  const toggleFolderFold = useCallback((folderPath: string) => {
    if (!workspace) return;
    dispatch({ type: 'difference_folder_fold_toggled', resultKey: workspace.key, folderPath });
  }, [dispatch, workspace]);

  const toggleSort = useCallback((key: SortKey) => {
    if (!workspace) return;
    const { natural } = keySpec(key);
    const nextSort: Sort | null = !sort || sort.key !== key
      ? { key, dir: natural }
      : sort.dir === natural
        ? { key, dir: sort.dir === 1 ? -1 : 1 }
        : null;
    dispatch({ type: 'difference_sort_changed', resultKey: workspace.key, sort: nextSort });
  }, [dispatch, sort, workspace]);

  const openRowMenu = useCallback((index: number, x: number, y: number) => {
    if (!plan) return;
    const operation = effectiveOperation(plan, reversedRows, index);
    const [sourcePath, targetPath] = sidePaths(operation);
    const sourceAbsolutePath = sourcePath ? joinDisplayPath(plan.header.source_root, sourcePath) : null;
    const targetAbsolutePath = targetPath ? joinDisplayPath(plan.header.target_root, targetPath) : null;
    const relativePath = operation.path;
    const baseName = relativePathBaseName(relativePath);
    const extensionSeparator = baseName.lastIndexOf('.');
    const extension = extensionSeparator > 0 ? baseName.slice(extensionSeparator + 1) : '';
    const folderPath = owningFolderOf(operation);
    const inFolderScope = inScopeIndices.filter((candidateIndex) => (
      matchesFolderScope(effectiveOperation(plan, reversedRows, candidateIndex), folderPath)
      && isExecutableOperation(effectiveOperation(plan, reversedRows, candidateIndex))
    ));
    const copyPath = (path: string) => navigator.clipboard?.writeText(path).then(
      () => setStatus(`Copied: ${path}`),
      () => setStatus('Copy failed (clipboard unavailable)', 'err'),
    );

    setContextMenu({
      x,
      y,
      entries: [
        {
          label: 'Show in File Manager · Source',
          disabled: !sourceAbsolutePath,
          run: () => {
            ipc.revealCompareRow(plan.owner.identity, index, 'source', reversedRows[index] === true)
              .catch((error) => setStatus(`Could not reveal the source item: ${error}`, 'err'));
          },
        },
        {
          label: 'Show in File Manager · Target',
          disabled: !targetAbsolutePath,
          run: () => {
            ipc.revealCompareRow(plan.owner.identity, index, 'target', reversedRows[index] === true)
              .catch((error) => setStatus(`Could not reveal the target item: ${error}`, 'err'));
          },
        },
        { separator: true, label: '' },
        { label: 'Copy Full Path', run: () => copyPath((sourceAbsolutePath ?? targetAbsolutePath)!) },
        { label: 'Copy Relative Path', run: () => copyPath(relativePath) },
        { separator: true, label: '' },
        {
          label: extension ? `Exclude This Type */*.${extension}` : 'Exclude This Type (No Extension)',
          disabled: !extension || !selectedJob,
          run: () => { void addExcludes([`*/*.${extension}`], 'Added to exclude'); },
        },
        {
          label: folderPath ? `Exclude This Directory /${folderPath}/` : 'Exclude This Directory (Already at the Root)',
          disabled: !folderPath || !selectedJob,
          run: () => { void addExcludes([`/${folderPath}/`], 'Added to exclude'); },
        },
        { separator: true, label: '' },
        {
          label: reversedRows[index] ? 'Restore Original Direction' : 'Reverse This Row',
          disabled: !reviewEditable || !canReverseOperation(plan, index),
          run: () => toggleRowDirection(index),
        },
        {
          label: 'Include Only This Item',
          disabled: !reviewEditable,
          run: () => replaceIncludedRows(plan.ops.map((_, candidateIndex) => (
            candidateIndex === index
            && isExecutableOperation(effectiveOperation(plan, reversedRows, candidateIndex))
          ))),
        },
        {
          label: `${folderPath ? 'Exclude This Folder and Subfolders' : 'Exclude Root-Level Items'} (${inFolderScope.length})`,
          disabled: !reviewEditable || inFolderScope.length === 0,
          run: () => setRowsIncluded(inFolderScope, false),
        },
      ],
    });
  }, [
    addExcludes,
    inScopeIndices,
    plan,
    replaceIncludedRows,
    reviewEditable,
    reversedRows,
    selectedJob,
    setContextMenu,
    setRowsIncluded,
    setStatus,
    toggleRowDirection,
  ]);

  return {
    openRowMenu,
    replaceIncludedRows,
    setRowIncluded,
    setRowsIncluded,
    toggleFolderFold,
    toggleRowDirection,
    toggleSort,
  };
}
