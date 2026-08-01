// Display order for the diff table: which rows, in what sequence, under which directory nodes.
//
// filter.ts decides membership and returns plan order. This file is the only display sequencer: it
// builds a directory trie, sorts rows and sibling folders, and flattens the result for the virtual
// table. finalIdx() deliberately ignores this order because apply must keep the engine's order (a
// directory delete has to follow the children inside it).

import { baseOf, dirOf } from './format.ts';
import { eff, keySpec, selectable, sortVal } from './plan.ts';
import type { OpDto, PlanDto, Sort } from './plan.ts';

/// The structural folder that owns one operation. A directory deletion targets the directory
/// itself, while every other operation targets an entry inside its parent. Sharing this rule with
/// the table and context menu keeps folder selection, display, and commands on the same scope.
export function treeDirOf(op: OpDto): string {
  return op.action === 'delete_dir' ? op.path : dirOf(op.path);
}

/// One rendered line. Normal rows stay bare plan indices: at hundreds of thousands of operations,
/// allocating an object per file would erase much of the virtual table's memory saving. Folder
/// rows are few enough to carry their presentation and subtree-range metadata.
export type RowSpec =
  | number
  | {
    kind: 'folder';
    /// Stable full relative path. Empty string is the compatibility bucket for root-level files.
    dir: string;
    depth: number;
    /// The folder's descendants occupy one range in PlanLayout.order. This lets a parent checkbox
    /// address its whole visible subtree without copying every descendant index onto every ancestor.
    start: number;
    end: number;
    count: number;
    selectable: number;
    bytes: number;
  };

/// One node in the directory trie. `direct` owns every row exactly once; aggregates cover the
/// complete visible subtree. The synthetic forest root is not represented. A `dir === ''` node is
/// shown only when root-level files exist and remains a sibling of real top-level folders, matching
/// the old `(root)` group rather than turning it into a select/collapse handle for the whole tree.
export interface FolderNode {
  dir: string;
  depth: number;
  direct: number[];
  children: FolderNode[];
  first: number;
  start: number;
  end: number;
  count: number;
  selectable: number;
  bytes: number;
}

interface WorkFolder extends FolderNode {
  id: number;
  children: WorkFolder[];
}

export interface PlanLayout {
  /// Visible rows in display order — what CSV exports and flattenLayout walks.
  order: number[];
  /// null when the view is flat; otherwise the roots of the real directory tree.
  tree: FolderNode[] | null;
}

export interface LayoutInput {
  plan: PlanDto;
  flipped: boolean[];
  /// From computeVisible(): membership, in plan order.
  visible: number[];
  grouped: boolean;
  sort: Sort | null;
}

/// Ranks two entries by precomputed keys. A missing side sorts last in both directions, while the
/// caller supplies a stable plan-order tie-break.
type Rank = (a: number, b: number) => number;

function numRank(miss: ArrayLike<number>, num: ArrayLike<number>, sign: 1 | -1): Rank {
  return (a, b) => (miss[a] - miss[b]) || (num[a] - num[b]) * sign;
}

function textRank(miss: ArrayLike<number>, text: ArrayLike<string>, sign: 1 | -1): Rank {
  return (a, b) => (miss[a] - miss[b]) || (text[a] < text[b] ? -sign : text[a] > text[b] ? sign : 0);
}

/// Rows in display order, plus a directory forest when tree grouping is on.
///
/// Explicit sorts are hierarchical: direct rows are sorted inside their folder, and child folders
/// are sorted among their siblings using a fold over each complete subtree. Without a sort, first
/// appearance in plan order remains the ordering contract.
export function buildLayout(inp: LayoutInput): PlanLayout {
  const { plan, flipped, visible, grouped, sort } = inp;
  const n = plan.ops.length;

  const isText = !!sort && keySpec(sort.key).kind === 'text';
  const rowMiss = new Int8Array(sort ? n : 0);
  const rowNum = new Float64Array(sort && !isText ? n : 0);
  const rowText: string[] = new Array(sort && isText ? n : 0);
  if (sort) {
    for (const i of visible) {
      const [miss, num, text] = sortVal(plan, flipped, i, sort.key);
      rowMiss[i] = miss;
      if (isText) rowText[i] = text; else rowNum[i] = num;
    }
  }
  const rankRow: Rank | null = !sort ? null
    : isText ? textRank(rowMiss, rowText, sort.dir) : numRank(rowMiss, rowNum, sort.dir);
  const cmpRow = rankRow && ((a: number, b: number) => rankRow(a, b) || a - b);

  if (!grouped) {
    if (!cmpRow) return { order: visible, tree: null };
    return { order: [...visible].sort(cmpRow), tree: null };
  }

  const roots: WorkFolder[] = [];
  const byDir = new Map<string, WorkFolder>();
  let nextId = 0;

  const makeFolder = (dir: string, depth: number, first: number, parent: WorkFolder | null): WorkFolder => {
    const node: WorkFolder = {
      id: nextId++, dir, depth, direct: [], children: [], first,
      start: 0, end: 0, count: 0, selectable: 0, bytes: 0,
    };
    byDir.set(dir, node);
    if (parent) parent.children.push(node); else roots.push(node);
    return node;
  };

  /// Ensures every ancestor of `dir` exists exactly once. The empty path is intentionally a leaf
  /// pseudo-folder for root files, never the parent of the real top-level nodes.
  const ensureFolder = (dir: string, first: number): WorkFolder => {
    // The dominant large-tree case is many files sharing one directory. Avoid splitting and
    // rebuilding every prefix for every one of those rows after the node already exists.
    const exact = byDir.get(dir);
    if (exact) {
      if (first < exact.first) exact.first = first;
      return exact;
    }
    if (dir === '') {
      return makeFolder('', 0, first, null);
    }
    let parent: WorkFolder | null = null;
    let full = '';
    const parts = dir.split('/');
    for (let depth = 0; depth < parts.length; depth++) {
      full = full ? `${full}/${parts[depth]}` : parts[depth];
      let node = byDir.get(full);
      if (!node) node = makeFolder(full, depth, first, parent);
      if (first < node.first) node.first = first;
      parent = node;
    }
    return parent!;
  };

  for (const i of visible) {
    const owner = ensureFolder(treeDirOf(eff(plan, flipped, i)), i);
    owner.direct.push(i);
  }

  // Folder sort aggregates are held once in compact parallel arrays. Keeping a descendant-index
  // array on every ancestor would grow as rows x path depth on exactly the large trees that need
  // virtualisation most.
  const folderMiss = new Int8Array(nextId);
  const folderNum = new Float64Array(sort && !isText ? nextId : 0);
  const folderText: string[] = new Array(sort && isText ? nextId : 0);
  const fold = sort ? keySpec(sort.key).fold : null;
  const rankFolder: Rank | null = !sort ? null
    : (isText || fold === 'dir')
      ? textRank(folderMiss, folderText, sort.dir)
      : numRank(folderMiss, folderNum, sort.dir);

  // Explicit postorder: VFS and long-path inputs can be thousands of directories deep, beyond the
  // JavaScript call stack even though the path itself is valid for that backend.
  const postorder: WorkFolder[] = [];
  const pending: WorkFolder[] = [...roots];
  while (pending.length > 0) {
    const node = pending.pop()!;
    postorder.push(node);
    for (const child of node.children) pending.push(child);
  }

  for (let p = postorder.length - 1; p >= 0; p--) {
    const node = postorder[p];
    if (cmpRow) node.direct.sort(cmpRow);

    // `first` was maintained while the trie was built. Do not derive it with
    // `Math.min(...direct)`: one directory can legitimately hold hundreds of thousands of rows,
    // and spreading that array would overflow the JS call stack.
    let first = node.first;
    let count = node.direct.length;
    let selectableCount = 0;
    let bytes = 0;
    for (const i of node.direct) {
      const op = eff(plan, flipped, i);
      if (selectable(op)) selectableCount++;
      bytes += op.size ?? 0;
    }
    for (const child of node.children) {
      first = Math.min(first, child.first);
      count += child.count;
      selectableCount += child.selectable;
      bytes += child.bytes;
    }
    node.first = first;
    node.count = count;
    node.selectable = selectableCount;
    node.bytes = bytes;

    if (sort && fold) {
      if (fold === 'dir') {
        // Siblings share the same parent prefix, so their segment is the meaningful tree key.
        folderText[node.id] = node.dir === '' ? '' : baseOf(node.dir).toLowerCase();
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
        for (const i of node.direct) take(rowMiss[i], rowNum[i], rowText[i]);
        for (const child of node.children) {
          take(folderMiss[child.id], folderNum[child.id], folderText[child.id]);
        }
        folderMiss[node.id] = missing;
        if (isText) folderText[node.id] = text; else folderNum[node.id] = missing ? 0 : num;
      }
    }

    node.children.sort((a, b) => (rankFolder?.(a.id, b.id) ?? 0) || a.first - b.first);
  }

  roots.sort((a, b) => (rankFolder?.(a.id, b.id) ?? 0) || a.first - b.first);

  // One shared DFS order makes every subtree a compact range. It powers CSV order, recursive
  // checkbox state, and rendering while storing each operation index only once.
  const order: number[] = [];
  const preorder: WorkFolder[] = [];
  for (let r = roots.length - 1; r >= 0; r--) preorder.push(roots[r]);
  while (preorder.length > 0) {
    const node = preorder.pop()!;
    node.start = order.length;
    // Do not spread: a single flat directory can hold more rows than V8 accepts as call arguments.
    for (const i of node.direct) order.push(i);
    // DFS makes the subtree contiguous, and count was already aggregated bottom-up.
    node.end = node.start + node.count;
    for (let c = node.children.length - 1; c >= 0; c--) preorder.push(node.children[c]);
  }

  return { order, tree: roots };
}

/// Flattens the directory forest into the virtual table's line list. Collapsing one node suppresses
/// both its direct rows and every descendant node, while leaving check state and layout.order intact.
export function flattenLayout(layout: PlanLayout, collapsed: ReadonlySet<string>): RowSpec[] {
  if (!layout.tree) return layout.order;
  const out: RowSpec[] = [];
  const pending: FolderNode[] = [];
  for (let r = layout.tree.length - 1; r >= 0; r--) pending.push(layout.tree[r]);
  while (pending.length > 0) {
    const node = pending.pop()!;
    out.push({
      kind: 'folder', dir: node.dir, depth: node.depth,
      start: node.start, end: node.end, count: node.count,
      selectable: node.selectable, bytes: node.bytes,
    });
    if (collapsed.has(node.dir)) continue;
    for (const i of node.direct) out.push(i);
    for (let c = node.children.length - 1; c >= 0; c--) pending.push(node.children[c]);
  }
  return out;
}

/// Every currently shown directory key, recursively, for collapse/expand-all and stale-state checks.
export function layoutDirs(layout: PlanLayout): string[] {
  if (!layout.tree) return [];
  const out: string[] = [];
  const pending: FolderNode[] = [];
  for (let r = layout.tree.length - 1; r >= 0; r--) pending.push(layout.tree[r]);
  while (pending.length > 0) {
    const node = pending.pop()!;
    out.push(node.dir);
    for (let c = node.children.length - 1; c >= 0; c--) pending.push(node.children[c]);
  }
  return out;
}
