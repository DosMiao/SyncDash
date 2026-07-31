import { useEffect, useLayoutEffect, useMemo, useState } from 'react';
import { projectVirtualGeometry } from './virtualGeometry';
import type { RefObject } from 'react';
import type { RowSpec } from '../../core/grouping';

// Virtual scrolling for the diff table body.
//
// One diff row = <tr> + up to 9 <td> + checkbox + action cell. Thousands of those in a live <table> cost
// seconds just to build and to recompute column widths — and a chip switch, a keystroke in the search box,
// or folding one directory redoes all of it. So we mount only the viewport's rows inside a bounded
// scroll canvas: render cost and browser-facing geometry both stay finite.
//
// Row heights are measured from the live DOM rather than hard-coded. The measurement runs when a row
// layout is mounted or replaced, so CSS changes do not silently make the logical offsets drift.
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
  /// Physical coordinate for the translated one-screen body table.
  bodyTop: number;
  /// Bounded height assigned to the actual DOM canvas.
  canvasHeight: number;
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
    for (let k = 0; k < n; k++) {
      arr[k] = y;
      y += typeof rowPlan[k] === 'number' ? metrics.row : metrics.grp;
    }
    arr[n] = y;
    return arr;
  }, [rowPlan, metrics.row, metrics.grp]);

  // A ResizeObserver rather than a window resize listener: the table also changes height when the log
  // panel opens or the compare panel closes, and neither of those is a window resize.
  useEffect(() => {
    const el = wrap;
    if (!el) return;
    let raf: number | null = null;
    const onScroll = () => {
      if (raf !== null) return;
      raf = requestAnimationFrame(() => {
        raf = null;
        const top = el.scrollTop;
        setView((v) => (v.top === top ? v : { ...v, top }));
      });
    };
    const ro = new ResizeObserver(() => {
      const height = el.clientHeight;
      setView((v) => (v.height === height ? v : { ...v, height }));
    });
    el.addEventListener('scroll', onScroll, { passive: true });
    ro.observe(el);
    setView({ top: el.scrollTop, height: el.clientHeight });
    return () => {
      el.removeEventListener('scroll', onScroll);
      ro.disconnect();
      if (raf !== null) cancelAnimationFrame(raf);
    };
  }, [wrap]);

  // Measure once for each row layout. This effect deliberately does not run after its own metrics
  // update: React 19 can keep a layout-effect state update in the current commit lane, so merely
  // returning the old state from the updater is not enough to prevent a nested-update loop (#185).
  // Zooming the webview does not change CSS-pixel offsetHeight, while every operation that can change
  // which row shapes exist replaces rowPlan and triggers a fresh measurement.
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
  }, [rowPlan]);

  const n = rowPlan.length;
  if (n === 0) return { from: 0, to: 0, bodyTop: metrics.head, canvasHeight: metrics.head };

  const geometry = projectVirtualGeometry({
    logicalBodyHeight: rowTop[n],
    headHeight: metrics.head,
    viewportHeight: view.height,
    scrollTop: view.top,
  });
  const top = geometry.logicalBodyTop;
  const from = Math.max(0, rowAt(rowTop, n, top) - OVERSCAN);
  const to = Math.min(n, rowAt(rowTop, n, top + view.height) + 1 + OVERSCAN);

  // Align the selected logical row with its projected physical viewport position. When the list is
  // shorter than the cap, physicalScroll === logicalScroll and this reduces to `head + rowTop[from]`.
  // On a huge list the table still moves only within the bounded canvas.
  const bodyTop = Math.round(
    geometry.physicalScroll + metrics.head + rowTop[from] - geometry.logicalScroll,
  );
  return { from, to, bodyTop, canvasHeight: geometry.canvasHeight };
}
