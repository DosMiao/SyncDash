import { useEffect, useLayoutEffect, useMemo, useState } from 'react';
import type { RefObject } from 'react';
import type { RowSpec } from '../../core/grouping';

// Virtual scrolling for the diff table body.
//
// One diff row = <tr> + up to 9 <td> + checkbox + action cell. Thousands of those in a live <table> cost
// seconds just to build and to recompute column widths — and a chip switch, a keystroke in the search box,
// or folding one directory redoes all of it. So we mount only the viewport's rows and pad above and below
// with spacer rows that stretch the scrollbar to the real height: render cost drops from O(whole table) to
// O(one screen).
//
// Row heights are measured from the live DOM rather than hard-coded, so a change to the type scale or to
// the row padding — or the user zooming the whole webview — corrects itself on the next frame instead of
// drifting rows out of place.
//
// core/grouping.ts decides what the lines are; this hook only decides which of them are on screen.

const OVERSCAN = 8; // extra rows above and below the viewport so fast scrolling never shows blanks

/// Binary search: the last row whose top is ≤ y
function rowAt(rowTop: Float64Array, n: number, y: number): number {
  let lo = 0, hi = n - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (rowTop[mid] <= y) lo = mid; else hi = mid - 1;
  }
  return lo;
}

export interface VirtualWindow {
  from: number;
  to: number;
  padTop: number;
  padBottom: number;
}

/// `wrap` is the scroll container, which the table does not own — it is handed down from whoever
/// rendered it. Passed as the element rather than a ref: a ref object's identity never changes, so
/// an effect depending on it would not re-run when `.current` goes from null to the real node, and
/// on mount it is always null (a child's effects run before an ancestor host ref attaches).
export function useVirtualRows(
  rowPlan: RowSpec[],
  wrap: HTMLElement | null,
  theadRef: RefObject<HTMLElement | null>,
  bodyRef: RefObject<HTMLElement | null>,
): VirtualWindow {
  const [metrics, setMetrics] = useState({ row: 34, grp: 36, head: 0 });
  const [view, setView] = useState({ top: 0, height: 600 });

  // Prefix sums: rowTop[k] = top offset of row k, rowTop[n] = total height
  const rowTop = useMemo(() => {
    const n = rowPlan.length;
    const arr = new Float64Array(n + 1);
    let y = 0;
    for (let k = 0; k < n; k++) { arr[k] = y; y += rowPlan[k].kind === 'grp' ? metrics.grp : metrics.row; }
    arr[n] = y;
    return arr;
  }, [rowPlan, metrics.row, metrics.grp]);

  // A ResizeObserver rather than a window resize listener: the table also changes height when the log
  // panel opens or the compare panel closes, and neither of those is a window resize.
  useEffect(() => {
    const el = wrap;
    if (!el) return;
    let pending = false;
    const onScroll = () => {
      if (pending) return;
      pending = true;
      requestAnimationFrame(() => { pending = false; setView((v) => ({ ...v, top: el.scrollTop })); });
    };
    const ro = new ResizeObserver(() => setView((v) => ({ ...v, height: el.clientHeight })));
    el.addEventListener('scroll', onScroll, { passive: true });
    ro.observe(el);
    setView({ top: el.scrollTop, height: el.clientHeight });
    return () => { el.removeEventListener('scroll', onScroll); ro.disconnect(); };
  }, [wrap]);

  // Measure after every commit. Returning the identical state object when nothing moved is what keeps
  // this from looping: React bails out of the re-render, so there is no measure → setState → measure cycle.
  useLayoutEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    const r = body.querySelector('tr:not(.vspacer):not(.grp)') as HTMLElement | null;
    const g = body.querySelector('tr.grp') as HTMLElement | null;
    const head = theadRef.current?.offsetHeight ?? 0;
    setMetrics((m) => {
      const row = r?.offsetHeight || m.row;
      const grp = g?.offsetHeight || m.grp;
      if (Math.abs(row - m.row) < 0.5 && Math.abs(grp - m.grp) < 0.5 && Math.abs(head - m.head) < 0.5) return m;
      return { row, grp, head };
    });
  });

  const n = rowPlan.length;
  if (n === 0) return { from: 0, to: 0, padTop: 0, padBottom: 0 };

  // Inside the scrolled content, tbody starts at the height of the sticky header
  const top = Math.max(0, view.top - metrics.head);
  const from = Math.max(0, rowAt(rowTop, n, top) - OVERSCAN);
  const to = Math.min(n, rowAt(rowTop, n, top + view.height) + 1 + OVERSCAN);
  return { from, to, padTop: rowTop[from], padBottom: rowTop[n] - rowTop[to] };
}
