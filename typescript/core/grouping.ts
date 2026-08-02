// Display order for the diff table: which rows, in what sequence, under which directory nodes.
//
// runScope.ts decides membership and returns plan order. This file is the only display sequencer: it
// builds a directory trie, sorts rows and sibling folders, and flattens the result for the virtual
// table. Apply deliberately ignores this order because directory deletion must follow its children.

import { relativePathBaseName } from './format.ts';
import { owningFolderOf, ROOT_FOLDER_PATH } from './folders.ts';
import { effectiveOperation, keySpec, isExecutableOperation, sortValue } from './plan.ts';
import type { PlanDto, Sort } from './plan.ts';

/// One rendered line. Normal rows stay bare plan indices: at hundreds of thousands of operations,
/// allocating an object per file would erase much of the virtual table's memory saving. Folder
/// rows are few enough to carry their presentation and subtree-range metadata.
export type RowSpec =
  | number
  | {
    kind: 'folder';
    /// Stable full relative path. Empty string is the first-class root-level result bucket.
    folderPath: string;
    depth: number;
    /// The folder's descendants occupy one range in PlanLayout.displayOrder. This lets a parent checkbox
    /// address its whole in-scope subtree without copying every descendant index onto every ancestor.
    start: number;
    end: number;
    count: number;
    executableCount: number;
    bytes: number;
  };

/// One node in the directory trie. `directIndices` owns every row exactly once; aggregates cover the
/// complete in-scope subtree. The synthetic forest root is not represented. A root-path node is
/// shown only when root-level files exist and remains a sibling of real top-level folders, rather
/// than becoming a select/collapse handle for the whole tree.
export interface FolderNode {
  path: string;
  depth: number;
  directIndices: number[];
  children: FolderNode[];
  firstPlanIndex: number;
  start: number;
  end: number;
  count: number;
  executableCount: number;
  bytes: number;
}

interface WorkFolder extends FolderNode {
  id: number;
  children: WorkFolder[];
}

export interface PlanLayout {
  /// In-scope rows in display order — what CSV exports and flattenLayout walks.
  displayOrder: number[];
  /// null when the view is flat; otherwise the roots of the real directory tree.
  folderTree: FolderNode[] | null;
}

export interface LayoutInput {
  plan: PlanDto;
  reversedRows: boolean[];
  /// From computeInScopeIndices(): membership, in plan order.
  inScopeIndices: number[];
  grouped: boolean;
  sort: Sort | null;
}

/// Ranks two entries by precomputed keys. A missing side sorts last in both directions, while the
/// caller supplies a stable plan-order tie-break.
type Rank = (left: number, right: number) => number;

function numericRank(
  missing: ArrayLike<number>,
  numericValues: ArrayLike<number>,
  direction: 1 | -1,
): Rank {
  return (left, right) => (
    (missing[left] - missing[right])
    || (numericValues[left] - numericValues[right]) * direction
  );
}

function textRank(
  missing: ArrayLike<number>,
  textValues: ArrayLike<string>,
  direction: 1 | -1,
): Rank {
  return (left, right) => (
    (missing[left] - missing[right])
    || (textValues[left] < textValues[right]
      ? -direction
      : textValues[left] > textValues[right]
        ? direction
        : 0)
  );
}

/// Rows in display order, plus a directory forest when tree grouping is on.
///
/// Explicit sorts are hierarchical: direct rows are sorted inside their folder, and child folders
/// are sorted among their siblings using a fold over each complete subtree. Without a sort, first
/// appearance in plan order remains the ordering contract.
export function buildLayout(input: LayoutInput): PlanLayout {
  const { plan, reversedRows, inScopeIndices, grouped, sort } = input;
  const operationCount = plan.ops.length;

  const isText = !!sort && keySpec(sort.key).kind === 'text';
  const rowMissing = new Int8Array(sort ? operationCount : 0);
  const rowNumber = new Float64Array(sort && !isText ? operationCount : 0);
  const rowText: string[] = new Array(sort && isText ? operationCount : 0);
  if (sort) {
    for (const index of inScopeIndices) {
      const [missing, numericValue, textValue] = sortValue(plan, reversedRows, index, sort.key);
      rowMissing[index] = missing;
      if (isText) rowText[index] = textValue; else rowNumber[index] = numericValue;
    }
  }
  const rankRow: Rank | null = !sort ? null
    : isText
      ? textRank(rowMissing, rowText, sort.dir)
      : numericRank(rowMissing, rowNumber, sort.dir);
  const compareRows = rankRow && ((left: number, right: number) => (
    rankRow(left, right) || left - right
  ));

  if (!grouped) {
    if (!compareRows) return { displayOrder: inScopeIndices, folderTree: null };
    return { displayOrder: [...inScopeIndices].sort(compareRows), folderTree: null };
  }

  const roots: WorkFolder[] = [];
  const folderByPath = new Map<string, WorkFolder>();
  let nextId = 0;

  const makeFolder = (
    path: string,
    depth: number,
    firstPlanIndex: number,
    parent: WorkFolder | null,
  ): WorkFolder => {
    const node: WorkFolder = {
      id: nextId++, path, depth, directIndices: [], children: [], firstPlanIndex,
      start: 0, end: 0, count: 0, executableCount: 0, bytes: 0,
    };
    folderByPath.set(path, node);
    if (parent) parent.children.push(node); else roots.push(node);
    return node;
  };

  /// Ensures every ancestor of `folderPath` exists exactly once. The empty path is intentionally a leaf
  /// pseudo-folder for root files, never the parent of the real top-level nodes.
  const ensureFolder = (folderPath: string, firstPlanIndex: number): WorkFolder => {
    // The dominant large-tree case is many files sharing one directory. Avoid splitting and
    // rebuilding every prefix for every one of those rows after the node already exists.
    const exact = folderByPath.get(folderPath);
    if (exact) {
      if (firstPlanIndex < exact.firstPlanIndex) exact.firstPlanIndex = firstPlanIndex;
      return exact;
    }
    if (folderPath === ROOT_FOLDER_PATH) {
      return makeFolder(ROOT_FOLDER_PATH, 0, firstPlanIndex, null);
    }
    let parent: WorkFolder | null = null;
    let full = '';
    const parts = folderPath.split('/');
    for (let depth = 0; depth < parts.length; depth++) {
      full = full ? `${full}/${parts[depth]}` : parts[depth];
      let node = folderByPath.get(full);
      if (!node) node = makeFolder(full, depth, firstPlanIndex, parent);
      if (firstPlanIndex < node.firstPlanIndex) node.firstPlanIndex = firstPlanIndex;
      parent = node;
    }
    return parent!;
  };

  for (const index of inScopeIndices) {
    const owner = ensureFolder(owningFolderOf(effectiveOperation(plan, reversedRows, index)), index);
    owner.directIndices.push(index);
  }

  // Folder sort aggregates are held once in compact parallel arrays. Keeping a descendant-index
  // array on every ancestor would grow as rows x path depth on exactly the large trees that need
  // virtualization most.
  const folderMissing = new Int8Array(nextId);
  const folderNumber = new Float64Array(sort && !isText ? nextId : 0);
  const folderText: string[] = new Array(sort && isText ? nextId : 0);
  const fold = sort ? keySpec(sort.key).fold : null;
  const rankFolder: Rank | null = !sort ? null
    : (isText || fold === 'dir')
      ? textRank(folderMissing, folderText, sort.dir)
      : numericRank(folderMissing, folderNumber, sort.dir);

  // Explicit postorder: VFS and long-path inputs can be thousands of directories deep, beyond the
  // JavaScript call stack even though the path itself is valid for that backend.
  const postorder: WorkFolder[] = [];
  const pending: WorkFolder[] = [...roots];
  while (pending.length > 0) {
    const node = pending.pop()!;
    postorder.push(node);
    for (const child of node.children) pending.push(child);
  }

  for (let position = postorder.length - 1; position >= 0; position--) {
    const node = postorder[position];
    if (compareRows) node.directIndices.sort(compareRows);

    // firstPlanIndex is maintained while building the trie. Spreading a very large directIndices
    // array into Math.min would overflow the JavaScript call stack.
    let firstPlanIndex = node.firstPlanIndex;
    let count = node.directIndices.length;
    let executableCount = 0;
    let bytes = 0;
    for (const index of node.directIndices) {
      const operation = effectiveOperation(plan, reversedRows, index);
      if (isExecutableOperation(operation)) executableCount++;
      bytes += operation.size ?? 0;
    }
    for (const child of node.children) {
      firstPlanIndex = Math.min(firstPlanIndex, child.firstPlanIndex);
      count += child.count;
      executableCount += child.executableCount;
      bytes += child.bytes;
    }
    node.firstPlanIndex = firstPlanIndex;
    node.count = count;
    node.executableCount = executableCount;
    node.bytes = bytes;

    if (sort && fold) {
      if (fold === 'dir') {
        // Siblings share the same parent prefix, so their segment is the meaningful tree key.
        folderText[node.id] = node.path === ROOT_FOLDER_PATH
          ? ROOT_FOLDER_PATH
          : relativePathBaseName(node.path).toLowerCase();
      } else {
        let missing = 1;
        let num = fold === 'sum' ? 0 : fold === 'min' ? Infinity : -Infinity;
        let text = '';
        const take = (miss: number, value: number, label: string): void => {
          if (miss) return;
          missing = 0;
          if (isText) {
            if (text === '' || (fold === 'min' ? label < text : label > text)) text = label;
          } else if (fold === 'sum') num += value;
          else if (fold === 'min') num = Math.min(num, value);
          else num = Math.max(num, value);
        };
        for (const index of node.directIndices) {
          take(rowMissing[index], rowNumber[index], rowText[index]);
        }
        for (const child of node.children) {
          take(folderMissing[child.id], folderNumber[child.id], folderText[child.id]);
        }
        folderMissing[node.id] = missing;
        if (isText) folderText[node.id] = text; else folderNumber[node.id] = missing ? 0 : num;
      }
    }

    node.children.sort((left, right) => (
      (rankFolder?.(left.id, right.id) ?? 0)
      || left.firstPlanIndex - right.firstPlanIndex
    ));
  }

  roots.sort((left, right) => (
    (rankFolder?.(left.id, right.id) ?? 0)
    || left.firstPlanIndex - right.firstPlanIndex
  ));

  // One shared DFS order makes every subtree a compact range. It powers CSV order, recursive
  // checkbox state, and rendering while storing each operation index only once.
  const displayOrder: number[] = [];
  const preorder: WorkFolder[] = [];
  for (let rootIndex = roots.length - 1; rootIndex >= 0; rootIndex--) {
    preorder.push(roots[rootIndex]);
  }
  while (preorder.length > 0) {
    const node = preorder.pop()!;
    node.start = displayOrder.length;
    // Do not spread: a single flat directory can hold more rows than V8 accepts as call arguments.
    for (const index of node.directIndices) displayOrder.push(index);
    // DFS makes the subtree contiguous, and count was already aggregated bottom-up.
    node.end = node.start + node.count;
    for (let childIndex = node.children.length - 1; childIndex >= 0; childIndex--) {
      preorder.push(node.children[childIndex]);
    }
  }

  return { displayOrder, folderTree: roots };
}

/// Flattens the directory forest into the virtual table's line list. Collapsing one node suppresses
/// both its direct rows and every descendant node, while leaving check state and display order intact.
export function flattenLayout(layout: PlanLayout, collapsed: ReadonlySet<string>): RowSpec[] {
  if (!layout.folderTree) return layout.displayOrder;
  const rows: RowSpec[] = [];
  const pending: FolderNode[] = [];
  for (let rootIndex = layout.folderTree.length - 1; rootIndex >= 0; rootIndex--) {
    pending.push(layout.folderTree[rootIndex]);
  }
  while (pending.length > 0) {
    const node = pending.pop()!;
    rows.push({
      kind: 'folder', folderPath: node.path, depth: node.depth,
      start: node.start, end: node.end, count: node.count,
      executableCount: node.executableCount, bytes: node.bytes,
    });
    if (collapsed.has(node.path)) continue;
    for (const index of node.directIndices) rows.push(index);
    for (let childIndex = node.children.length - 1; childIndex >= 0; childIndex--) {
      pending.push(node.children[childIndex]);
    }
  }
  return rows;
}

/// Every currently shown directory key, recursively, for collapse/expand-all and stale-state checks.
export function layoutFolderPaths(layout: PlanLayout): string[] {
  if (!layout.folderTree) return [];
  const folderPaths: string[] = [];
  const pending: FolderNode[] = [];
  for (let rootIndex = layout.folderTree.length - 1; rootIndex >= 0; rootIndex--) {
    pending.push(layout.folderTree[rootIndex]);
  }
  while (pending.length > 0) {
    const node = pending.pop()!;
    folderPaths.push(node.path);
    for (let childIndex = node.children.length - 1; childIndex >= 0; childIndex--) {
      pending.push(node.children[childIndex]);
    }
  }
  return folderPaths;
}
