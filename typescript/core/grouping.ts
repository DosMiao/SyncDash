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
export type RowSpec =
  | { kind: 'grp'; dir: string; items: number[]; sel: number[]; bytes: number }
  | { kind: 'row'; i: number; groupDir: string | null };

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

/// Shared stand-in so a numeric sort never allocates a per-plan string array. Never written to, and
/// only ever read on the text path, which always allocates a real one.
const EMPTY_STR: string[] = [];

/// Compare two precomputed key triples. `dir` scales the value comparison and nothing else — a
/// missing side sorts last in both directions, and ties resolve ascending whichever way you clicked,
/// so flipping the direction never scrambles rows that compare equal.
function cmp(
  miss: ArrayLike<number>, num: ArrayLike<number>, str: ArrayLike<string>,
  a: number, b: number, dir: 1 | -1,
): number {
  return (miss[a] - miss[b])
    || (num[a] - num[b]) * dir
    || (str[a] < str[b] ? -dir : str[a] > str[b] ? dir : 0);
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
  let miss: Int8Array | null = null, num: Float64Array | null = null, str: string[] | null = null;
  if (sort) {
    miss = new Int8Array(n);
    num = new Float64Array(n);
    // Only allocated for text keys: the numeric keys are the common ones, and filling 20k empty
    // strings on every size click is pure waste
    str = isText ? new Array(n) : EMPTY_STR;
    for (const i of visible) {
      const [a, b, c] = sortVal(plan, flipped, i, sort.key);
      miss[i] = a; num[i] = b;
      if (isText) str[i] = c;
    }
  }

  if (!grouped) {
    const order = sort ? [...visible] : visible;
    // `|| a - b` makes the comparator a total order rather than merely stable: same input, same
    // output, whatever the engine's sort does with equal elements
    if (sort) order.sort((a, b) => cmp(miss!, num!, str!, a, b, sort.dir) || a - b);
    return { order, groups: null };
  }

  // One group per directory — not per contiguous run. A directory holding both a copy and a delete
  // used to produce two group rows, because the engine ranks by action before path; merging fixes
  // that, and makes the group's dir a unique React key that survives a re-sort.
  const byDir = new Map<string, GroupSpec>();
  const groups: GroupSpec[] = [];
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

  if (sort) {
    const { dir, key } = sort;
    const { fold } = keySpec(key);
    for (const g of groups) g.items.sort((a, b) => cmp(miss!, num!, str!, a, b, dir) || a - b);

    // Fold each group's members into one value, then order the groups by it. Folds never depend on
    // `dir` — a group's aggregate has to be a property of the group, or "why is this folder above
    // that one" stops having an answer you can check.
    const d = groups.length;
    const gmiss = new Int8Array(d), gnum = new Float64Array(d);
    const gstr: string[] = new Array(d);
    for (let k = 0; k < d; k++) {
      const g = groups[k];
      if (fold === 'dir') {
        // A path column's group value is the directory itself: folding member paths is circular,
        // and would let one missing side sink a whole directory
        gmiss[k] = 0; gnum[k] = 0; gstr[k] = g.dir.toLowerCase();
        continue;
      }
      // A group has something to show in this column if *any* member does — the row-level rule,
      // lifted one level
      let m = 1, v = fold === 'sum' ? 0 : fold === 'min' ? Infinity : -Infinity;
      let s = '';
      for (const i of g.items) {
        if (miss![i]) continue;
        m = 0;
        if (fold === 'sum') v += num![i];
        else if (fold === 'min') v = Math.min(v, num![i]);
        else v = Math.max(v, num![i]);
        if (isText) {
          const t = str![i];
          if (s === '' || (fold === 'min' ? t < s : t > s)) s = t;
        }
      }
      gmiss[k] = m;
      gnum[k] = m ? 0 : v;
      gstr[k] = s;
    }
    const idx = groups.map((_, k) => k);
    idx.sort((a, b) => cmp(gmiss, gnum, gstr, a, b, dir) || groups[a].first - groups[b].first);
    const sorted = idx.map((k) => groups[k]);
    groups.length = 0;
    groups.push(...sorted);
  }

  const order: number[] = [];
  for (const g of groups) for (const i of g.items) order.push(i);
  return { order, groups };
}

/// The flat line list the table renders. Split from buildLayout so that folding a directory — which
/// changes only which member rows are emitted — costs one O(n) pass instead of redoing the sort.
export function flattenLayout(layout: PlanLayout, collapsed: ReadonlySet<string>): RowSpec[] {
  if (!layout.groups) return layout.order.map((i) => ({ kind: 'row', i, groupDir: null }));
  const out: RowSpec[] = [];
  for (const g of layout.groups) {
    out.push({ kind: 'grp', dir: g.dir, items: g.items, sel: g.sel, bytes: g.bytes });
    if (!collapsed.has(g.dir)) for (const i of g.items) out.push({ kind: 'row', i, groupDir: g.dir });
  }
  return out;
}

/// Every directory currently shown, for "Collapse all". Unique by construction — one group per dir.
export function layoutDirs(layout: PlanLayout): string[] {
  return layout.groups ? layout.groups.map((g) => g.dir) : [];
}
