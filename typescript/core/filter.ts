// Which rows the diff table currently shows.
//
// FFS semantics, carried over verbatim: **the view is the action set** — a row hidden by any of these
// filters does not run. That is why this lives in one place and why finalIdx() below intersects with it.

import { category, eff, rowMtime, rowSize, sortVal } from './plan';
import type { Chip, PlanDto, Sort } from './plan';

/// P3 funnel: a view filter over the **current compare result** (no rescan).
/// Mask matching goes back to Rust (filter::mask_hits); size/age are plain numbers and stay in the frontend.
export interface ViewFilter { masks: string[]; minMB: number | null; maxMB: number | null; days: number | null }

export const EMPTY_FILTER: ViewFilter = { masks: [], minMB: null, maxMB: null, days: null };

export interface VisibleInput {
  plan: PlanDto;
  flipped: boolean[];
  chips: Set<Chip>;
  search: string;
  ovFilter: string | null;
  vfilter: ViewFilter;
  /// Per-op "hit by a mask" result from Rust; an empty array means "no mask filtering"
  maskHit: boolean[];
  sort: Sort | null;
}

export function funnelActive(v: ViewFilter): number {
  return (v.masks.length ? 1 : 0)
    + (v.minMB !== null || v.maxMB !== null ? 1 : 0)
    + (v.days !== null ? 1 : 0);
}

function matchesSearch(path: string, from: string | null | undefined, reason: string, q: string): boolean {
  if (!q) return true;
  return path.toLowerCase().includes(q)
    || (from ?? '').toLowerCase().includes(q)
    || reason.toLowerCase().includes(q);
}

function matchesOv(path: string, ovFilter: string | null): boolean {
  if (!ovFilter) return true;
  if (ovFilter === '(root)') return !path.includes('/');
  return path === ovFilter || path.startsWith(ovFilter + '/');
}

/// The visible index list, in display order. One full-table scan plus an optional sort; callers memoize it.
export function computeVisible(inp: VisibleInput): number[] {
  const { plan, flipped, chips, ovFilter, vfilter, maskHit, sort } = inp;
  const n = plan.ops.length;
  const q = inp.search.toLowerCase();
  const MB = 1024 * 1024;
  const hasSize = vfilter.minMB !== null || vfilter.maxMB !== null;
  const cutoff = vfilter.days !== null ? Date.now() - vfilter.days * 86400_000 : null;

  const out: number[] = [];
  for (let i = 0; i < n; i++) {
    if (maskHit[i]) continue;
    const op = eff(plan, flipped, i);
    if (chips.size > 0 && !chips.has(category(op))) continue;
    if (!matchesSearch(op.path, op.from, op.reason, q)) continue;
    if (!matchesOv(op.path, ovFilter)) continue;
    if (hasSize) {
      const sz = rowSize(plan, flipped, i);
      if (vfilter.minMB !== null && sz < vfilter.minMB * MB) continue;
      if (vfilter.maxMB !== null && sz > vfilter.maxMB * MB) continue;
    }
    if (cutoff !== null) {
      const t = rowMtime(plan, i);
      if (!t || t < cutoff) continue;
    }
    out.push(i);
  }

  if (sort) {
    const { key, dir } = sort;
    // Precompute the sort keys once: the comparator runs n·log n times, and computing keys inside it
    // means hundreds of thousands of eff()/metaOf() calls plus string allocations
    const miss = new Int8Array(n), num = new Float64Array(n);
    const str: string[] = new Array(n);
    for (const i of out) { const [a, b, c] = sortVal(plan, flipped, i, key); miss[i] = a; num[i] = b; str[i] = c; }
    out.sort((a, b) =>
      (miss[a] - miss[b])                                            // missing always last, direction never flips it
      || (num[a] - num[b]) * dir
      || (str[a] < str[b] ? -dir : str[a] > str[b] ? dir : 0)
      || a - b);                                                     // ties keep the plan's original order
  }
  return out;
}

/// Final action set = checked ∩ currently visible.
/// This also closes a quiet hole in the pre-v0.9 behavior, where filtering with the search box still let
/// hidden rows ride along on Synchronize.
///
/// Returned in **plan order**, never in the view's sort order: this list is what apply_job executes, and
/// the plan's order is the engine's order (a directory delete has to follow the children inside it).
/// The CSV export deliberately uses the visible order instead — that one is a snapshot of the view.
export function finalIdx(visible: number[], checked: boolean[]): number[] {
  const vis = new Set(visible);
  const out: number[] = [];
  for (let i = 0; i < checked.length; i++) if (checked[i] && vis.has(i)) out.push(i);
  return out;
}
