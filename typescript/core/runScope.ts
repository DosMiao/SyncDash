// Run-scope membership is an execution boundary: an included row outside this module's result does
// not run. Display sorting, grouping, folding, and path formatting must never enter this data flow.

import { owningFolderOf, ROOT_FOLDER_PATH, ROOT_LEVEL_LABEL } from './folders.ts';
import {
  effectiveOperation,
  RESULT_TYPES,
  resultTypeOf,
  rowModifiedTime,
  rowSize,
  rowTransferBytes,
  isExecutableOperation,
} from './plan.ts';
import type { PlanDto, PlanOperation, ResultType } from './plan.ts';

export interface AdvancedScopeFilter {
  masks: string[];
  minimumMiB: number | null;
  maximumMiB: number | null;
  modifiedWithinDays: number | null;
}

export const EMPTY_ADVANCED_SCOPE_FILTER: AdvancedScopeFilter = {
  masks: [],
  minimumMiB: null,
  maximumMiB: null,
  modifiedWithinDays: null,
};

export function parseScopeMasks(text: string): string[] {
  return text.split('\n').map((value) => value.trim()).filter(Boolean);
}

export function isMaskMatchResult(value: unknown, rowCount: number): value is boolean[] {
  return Array.isArray(value)
    && value.length === rowCount
    && value.every((entry) => typeof entry === 'boolean');
}

export interface RunScopeInput {
  plan: PlanDto;
  reversedRows: boolean[];
  selectedResultTypes: ReadonlySet<ResultType>;
  searchQuery: string;
  folderScope: string | null;
  advancedFilter: AdvancedScopeFilter;
  excludedByMask: readonly boolean[];
}

export function countActiveAdvancedFilterGroups(filter: AdvancedScopeFilter): number {
  return (filter.masks.length > 0 ? 1 : 0)
    + (filter.minimumMiB !== null || filter.maximumMiB !== null ? 1 : 0)
    + (filter.modifiedWithinDays !== null ? 1 : 0);
}

function matchesSearch(operation: PlanOperation, query: string): boolean {
  if (!query) return true;
  return operation.path.toLowerCase().includes(query)
    || (operation.from ?? '').toLowerCase().includes(query)
    || operation.reason.toLowerCase().includes(query);
}

/// Empty string selects rows owned by the root level; null removes the folder constraint.
export function matchesFolderScope(operation: PlanOperation, folderScope: string | null): boolean {
  if (folderScope === null) return true;
  const owner = owningFolderOf(operation);
  if (folderScope === ROOT_FOLDER_PATH) return owner === ROOT_FOLDER_PATH;
  return owner === folderScope || owner.startsWith(`${folderScope}/`);
}

/// Returns membership in engine plan order. Apply relies on that order because directory deletion
/// must follow the operations on its descendants.
export function computeInScopeIndices(input: RunScopeInput): number[] {
  const {
    plan,
    reversedRows,
    selectedResultTypes,
    folderScope,
    advancedFilter,
    excludedByMask,
  } = input;
  const query = input.searchQuery.toLowerCase();
  const mebibyte = 1024 * 1024;
  const hasSizeConstraint = advancedFilter.minimumMiB !== null || advancedFilter.maximumMiB !== null;
  const modifiedCutoff = advancedFilter.modifiedWithinDays === null
    ? null
    : plan.header.generated_at_ms - advancedFilter.modifiedWithinDays * 86_400_000;

  const indices: number[] = [];
  for (let index = 0; index < plan.ops.length; index++) {
    if (excludedByMask[index] === true) continue;
    const operation = effectiveOperation(plan, reversedRows, index);
    if (selectedResultTypes.size > 0 && !selectedResultTypes.has(resultTypeOf(operation))) continue;
    if (!matchesSearch(operation, query)) continue;
    if (!matchesFolderScope(operation, folderScope)) continue;
    if (hasSizeConstraint) {
      const size = rowSize(plan, reversedRows, index);
      if (advancedFilter.minimumMiB !== null && size < advancedFilter.minimumMiB * mebibyte) continue;
      if (advancedFilter.maximumMiB !== null && size > advancedFilter.maximumMiB * mebibyte) continue;
    }
    if (modifiedCutoff !== null) {
      const modifiedAt = rowModifiedTime(plan, index);
      if (!modifiedAt || modifiedAt < modifiedCutoff) continue;
    }
    indices.push(index);
  }
  return indices;
}

export function computeExecutableIndices(
  plan: PlanDto,
  reversedRows: boolean[],
  inScopeIndices: readonly number[],
  includedRows: readonly boolean[],
): number[] {
  return inScopeIndices.filter((index) => (
    includedRows[index] === true && isExecutableOperation(effectiveOperation(plan, reversedRows, index))
  ));
}

export interface RunScopeFolderNode {
  path: string;
  label: string;
  firstPlanIndex: number;
  directCount: number;
  directTransferBytes: number;
  directDeletionBytes: number;
  resultCount: number;
  transferBytes: number;
  deletionBytes: number;
  children: RunScopeFolderNode[];
}

export interface RunScopeFolderRow {
  node: RunScopeFolderNode;
  depth: number;
}

export interface RunScopeModel {
  folders: RunScopeFolderNode[];
  countByResultType: Record<ResultType, number>;
}

function byteMetricKind(operation: PlanOperation): 'transfer' | 'deletion' | null {
  if (operation.action === 'copy' || operation.action === 'update') return 'transfer';
  if (operation.action === 'delete' || operation.action === 'delete_dir') return 'deletion';
  return null;
}

export function buildRunScopeModel(plan: PlanDto, reversedRows: boolean[]): RunScopeModel {
  const folders: RunScopeFolderNode[] = [];
  const nodes: RunScopeFolderNode[] = [];
  const nodeByPath = new Map<string, RunScopeFolderNode>();
  const countByResultType = Object.fromEntries(RESULT_TYPES.map((type) => [type, 0])) as Record<ResultType, number>;

  const createNode = (
    path: string,
    label: string,
    firstPlanIndex: number,
    parent: RunScopeFolderNode | null,
  ): RunScopeFolderNode => {
    const node: RunScopeFolderNode = {
      path,
      label,
      firstPlanIndex,
      directCount: 0,
      directTransferBytes: 0,
      directDeletionBytes: 0,
      resultCount: 0,
      transferBytes: 0,
      deletionBytes: 0,
      children: [],
    };
    nodes.push(node);
    nodeByPath.set(path, node);
    if (parent) parent.children.push(node); else folders.push(node);
    return node;
  };

  const ensureFolder = (path: string, firstPlanIndex: number): RunScopeFolderNode => {
    const exact = nodeByPath.get(path);
    if (exact) return exact;
    if (path === ROOT_FOLDER_PATH) {
      return createNode(ROOT_FOLDER_PATH, ROOT_LEVEL_LABEL, firstPlanIndex, null);
    }

    let parent: RunScopeFolderNode | null = null;
    let completePath = '';
    for (const segment of path.split('/')) {
      completePath = completePath ? `${completePath}/${segment}` : segment;
      let node = nodeByPath.get(completePath);
      if (!node) node = createNode(completePath, segment, firstPlanIndex, parent);
      parent = node;
    }
    return parent!;
  };

  for (let index = 0; index < plan.ops.length; index++) {
    const operation = effectiveOperation(plan, reversedRows, index);
    const resultType = resultTypeOf(operation);
    countByResultType[resultType] += 1;

    const owner = ensureFolder(owningFolderOf(operation), index);
    owner.directCount += 1;
    const metricKind = byteMetricKind(operation);
    if (metricKind === 'transfer') owner.directTransferBytes += rowTransferBytes(plan, reversedRows, index);
    if (metricKind === 'deletion') owner.directDeletionBytes += Math.max(0, operation.size ?? 0);
  }

  // Parent-before-child insertion makes this reverse pass a stack-safe postorder for VFS paths that
  // can exceed the JavaScript recursion limit.
  for (let index = nodes.length - 1; index >= 0; index--) {
    const node = nodes[index];
    node.resultCount += node.directCount;
    node.transferBytes += node.directTransferBytes;
    node.deletionBytes += node.directDeletionBytes;
    for (const child of node.children) {
      node.resultCount += child.resultCount;
      node.transferBytes += child.transferBytes;
      node.deletionBytes += child.deletionBytes;
    }
    node.children.sort((left, right) => (
      right.resultCount - left.resultCount
      || left.firstPlanIndex - right.firstPlanIndex
      || left.label.localeCompare(right.label)
    ));
  }
  folders.sort((left, right) => (
    right.resultCount - left.resultCount
    || left.firstPlanIndex - right.firstPlanIndex
    || left.label.localeCompare(right.label)
  ));

  return { folders, countByResultType };
}

export function flattenExpandedFolders(
  folders: readonly RunScopeFolderNode[],
  expandedFolders: ReadonlySet<string>,
): RunScopeFolderRow[] {
  const rows: RunScopeFolderRow[] = [];
  const pending: RunScopeFolderRow[] = [];
  for (let index = folders.length - 1; index >= 0; index--) {
    pending.push({ node: folders[index], depth: 0 });
  }
  while (pending.length > 0) {
    const row = pending.pop()!;
    rows.push(row);
    if (!expandedFolders.has(row.node.path)) continue;
    for (let index = row.node.children.length - 1; index >= 0; index--) {
      pending.push({ node: row.node.children[index], depth: row.depth + 1 });
    }
  }
  return rows;
}
