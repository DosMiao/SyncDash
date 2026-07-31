// Display order for the diff table: which rows, in what sequence, under which directory headings.
//
// This used to be split in two — filter.ts decided row order, App.tsx decided group order, and
// neither could see the other. The only way to keep them consistent was to forbid one of them, which
// is why sorting used to drop you out of the tree. Both live here now, so a sort can order rows
// *inside* each directory and order the directories themselves by the same key.
//
// filter.ts still decides membership, and it returns plan order. This file is the only thing that
// decides sequence — except finalIdx(), which deliberately ignores both: what a run executes is the
// engine's order, because a directory delete has to follow the children inside it.

import { dirOf } from './format';
import { eff, keySpec, selectable, sortVal } from './plan';
import type { PlanDto, Sort } from './plan';

/// One rendered line. A flat array of these is what the virtual scroller consumes: it measures one
/// row and one group row, so the two kinds must stay distinguishable and the array must stay flat.
// A normal row is just its plan index. Keeping `{ kind: 'row', i, groupDir }` for every visible
// item looked harmless at 20k rows, but a large compare can hold hundreds of thousands: the
// "virtual" table then retained one extra JS object per row before it rendered a single line.
// Group headers need metadata and stay objects; member rows recover their directory from the op.
export type RowSpec =
  | number
  | { kind: 'grp'; dir: string; items: number[]; sel: number[]; bytes: number };

export interface GroupSpec {
  dir: string;
  /// Every member, in display order
  items: number[];
  /// The members that can be checked — conflicts and notes are reports, so a group of only those
  /// has an empty `sel` and a disabled checkbox
  sel: number[];
  /// Σ of the members' op sizes: "how much this group's operations move". Deliberately *not* the
  /// same quantity as the s.size/t.size sort aggregate, which sums a measured side — the header you
  /// clicked names a side, while this line describes the operation.
  bytes: number;
  /// Lowest member index, i.e. where this directory first appears in plan order. Used as the
  /// unsorted group order and as the group comparator's tie-break, and unique because indices are.
  first: number;
}

export interface PlanLayout {
  /// Visible rows in display order — what the CSV exports and what flattenLayout walks
  order: number[];
  /// null when the view is flat (grouping off)
  groups: GroupSpec[] | null;
}

export interface LayoutInput {
  plan: PlanDto;
  flipped: boolean[];
  /// From computeVisible(): membership, in plan order
  visible: number[];
  grouped: boolean;
  sort: Sort | null;
}

/// Ranks two entries by their precomputed keys. `sign` scales the value comparison and nothing else:
/// a missing side sorts last in both directions, and the caller's index tie-break stays ascending
/// whichever way you clicked, so flipping direction never scrambles entries that compare equal.
///
/// A key is numeric *or* text, never both — `keySpec().kind` decides which array was filled — so the
/// two are separate functions rather than one that reads an array it knows is empty.
type Rank = (a: number, b: number) => number;

function numRank(miss: ArrayLike<number>, num: ArrayLike<number>, sign: 1 | -1): Rank {
  return (a, b) => (miss[a] - miss[b]) || (num[a] - num[b]) * sign;
}

function textRank(miss: ArrayLike<number>, text: ArrayLike<string>, sign: 1 | -1): Rank {
  return (a, b) => (miss[a] - miss[b]) || (text[a] < text[b] ? -sign : text[a] > text[b] ? sign : 0);
}

/// Rows in display order, plus the directory groups when grouping is on.
///
/// Bucket first, then sort. Sorting first and taking contiguous runs — which is what the old code
/// did — shatters every directory into singletons the moment you sort by anything but path. It is
/// also slower: bucketing is an O(n) pass you need regardless, and sorting inside 2000 small buckets
/// costs a fraction of one global sort over 20000 rows.
export function buildLayout(inp: LayoutInput): PlanLayout {
  const { plan, flipped, visible, grouped, sort } = inp;
  const n = plan.ops.length;

  // Key precompute, shared by both levels. Sized over the whole plan and filled only at visible
  // indices, so a row's key is addressable by its plan index with no indirection. Computing keys
  // inside the comparator instead would mean hundreds of thousands of eff()/metaOf() calls.
  const isText = !!sort && keySpec(sort.key).kind === 'text';
  const rowMiss = new Int8Array(sort ? n : 0);
  const rowNum = new Float64Array(sort && !isText ? n : 0);
  // Allocated only for text keys: the numeric keys are the common ones, and filling 20k empty
  // strings on every size click is pure waste
  const rowText: string[] = new Array(sort && isText ? n : 0);
  if (sort) {
    for (const i of visible) {
      const [miss, num, text] = sortVal(plan, flipped, i, sort.key);
      rowMiss[i] = miss;
      if (isText) rowText[i] = text; else rowNum[i] = num;
    }
  }
  // `|| a - b` makes each comparator a total order rather than merely stable: same input, same
  // output, whatever the engine's sort does with equal elements
  const rankRow: Rank | null = !sort ? null
    : isText ? textRank(rowMiss, rowText, sort.dir) : numRank(rowMiss, rowNum, sort.dir);
  const cmpRow = rankRow && ((a: number, b: number) => rankRow(a, b) || a - b);

  if (!grouped) {
    // `visible` is returned as-is when there is nothing to reorder: it is treated as immutable
    // everywhere, and copying 20k indices to hand back the same sequence buys nothing
    if (!cmpRow) return { order: visible, groups: null };
    return { order: [...visible].sort(cmpRow), groups: null };
  }

  // One group per directory — not per contiguous run. A directory holding both a copy and a delete
  // used to produce two group rows, because the engine ranks by action before path; merging fixes
  // that, and makes the group's dir a unique React key that survives a re-sort.
  const byDir = new Map<string, GroupSpec>();
  let groups: GroupSpec[] = [];
  for (const i of visible) {
    const op = eff(plan, flipped, i);
    const dir = dirOf(op.path);
    let g = byDir.get(dir);
    if (!g) {
      // `visible` is ascending, so the index that creates the group is also its minimum
      g = { dir, items: [], sel: [], bytes: 0, first: i };
      byDir.set(dir, g);
      groups.push(g);
    }
    g.items.push(i);
    if (selectable(op)) g.sel.push(i);
    g.bytes += op.size ?? 0;
  }

  if (sort && cmpRow) {
    const { fold } = keySpec(sort.key);
    for (const g of groups) g.items.sort(cmpRow);

    // Fold each group's members into one value, then order the groups by it. Folds never depend on
    // the direction — a group's aggregate has to be a property of the group, or "why is this folder
    // above that one" stops having an answer you can check.
    const nGroups = groups.length;
    const groupMiss = new Int8Array(nGroups);
    const groupNum = new Float64Array(isText ? 0 : nGroups);
    const groupText: string[] = new Array(isText || fold === 'dir' ? nGroups : 0);
    for (let k = 0; k < nGroups; k++) {
      const g = groups[k];
      if (fold === 'dir') {
        // A path column's group value is the directory itself: folding member paths is circular,
        // and would let one missing side sink a whole directory
        groupText[k] = g.dir.toLowerCase();
        continue;
      }
      // A group has something to show in this column if *any* member does — the row-level rule,
      // lifted one level
      let missing = 1;
      let value = fold === 'sum' ? 0 : fold === 'min' ? Infinity : -Infinity;
      let text = '';
      for (const i of g.items) {
        if (rowMiss[i]) continue;
        missing = 0;
        if (isText) {
          const t = rowText[i];
          if (text === '' || (fold === 'min' ? t < text : t > text)) text = t;
        } else if (fold === 'sum') value += rowNum[i];
        else if (fold === 'min') value = Math.min(value, rowNum[i]);
        else value = Math.max(value, rowNum[i]);
      }
      groupMiss[k] = missing;
      if (isText) groupText[k] = text; else groupNum[k] = missing ? 0 : value;
    }
    // `fold === 'dir'` writes a directory name, so it ranks as text whatever the key's own kind says
    const rankGroup = isText || fold === 'dir'
      ? textRank(groupMiss, groupText, sort.dir)
      : numRank(groupMiss, groupNum, sort.dir);
    // Pair each group with its index into the fold arrays, so the sort never has to read back into
    // the array it is producing. Tie-break on first appearance, unique because plan indices are.
    const ranked = groups.map((g, k) => ({ g, k }));
    ranked.sort((x, y) => rankGroup(x.k, y.k) || x.g.first - y.g.first);
    groups = ranked.map((r) => r.g);
  }

  const order: number[] = [];
  for (const g of groups) for (const i of g.items) order.push(i);
  return { order, groups };
}

/// The flat line list the table renders. Split from buildLayout so that folding a directory — which
/// changes only which member rows are emitted — costs one O(n) pass instead of redoing the sort.
export function flattenLayout(layout: PlanLayout, collapsed: ReadonlySet<string>): RowSpec[] {
  // `number[]` is already a valid RowSpec[] and is immutable by contract. In flat mode this avoids
  // both a full-size copy and one object allocation per visible operation.
  if (!layout.groups) return layout.order;
  const out: RowSpec[] = [];
  for (const g of layout.groups) {
    out.push({ kind: 'grp', dir: g.dir, items: g.items, sel: g.sel, bytes: g.bytes });
    if (!collapsed.has(g.dir)) for (const i of g.items) out.push(i);
  }
  return out;
}

/// Every directory currently shown, for "Collapse all". Unique by construction — one group per dir.
export function layoutDirs(layout: PlanLayout): string[] {
  return layout.groups ? layout.groups.map((g) => g.dir) : [];
}
