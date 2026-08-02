import { useLayoutEffect, useMemo, useRef, useState } from 'react';
import { projectLogicalScrollTop, projectVirtualGeometry } from '#ui/features/compare-results/model/virtualGeometry.ts';
import type { RefObject } from 'react';
import type { RowSpec } from '#core/domain/compare/grouping.ts';

// Row offsets stay in the logical result space while virtualGeometry projects them onto a bounded
// browser scroll canvas. Live DOM measurements keep that mapping aligned with the active row layout.
const OVERSCAN_ROWS = 8;

function rowIndexAtOffset(
  rowOffsets: Float64Array,
  rowCount: number,
  logicalOffset: number,
): number {
  let lowerBound = 0;
  let upperBound = rowCount - 1;
  while (lowerBound < upperBound) {
    const midpoint = (lowerBound + upperBound + 1) >> 1;
    if (rowOffsets[midpoint] <= logicalOffset) {
      lowerBound = midpoint;
    } else {
      upperBound = midpoint - 1;
    }
  }
  return lowerBound;
}

export interface VirtualWindow {
  from: number;
  to: number;
  /// Physical coordinate for the translated one-screen body table.
  bodyTop: number;
  /// Bounded height assigned to the actual DOM canvas.
  canvasHeight: number;
}

export interface ResultViewport {
  logicalTop: number;
  scrollLeft: number;
}

/// The scroll element is passed directly because a ref object's stable identity cannot signal when
/// its current element is attached or replaced.
export function useVirtualRows<OwnerKey extends string>(
  rowPlan: RowSpec[],
  wrap: HTMLElement | null,
  theadRef: RefObject<HTMLElement | null>,
  bodyRef: RefObject<HTMLElement | null>,
  ownerKey: OwnerKey,
  viewport: ResultViewport,
  onViewportChange: (ownerKey: OwnerKey, viewport: ResultViewport) => void,
): VirtualWindow {
  const [measuredHeights, setMeasuredHeights] = useState({
    operationRow: 34,
    groupRow: 36,
    header: 0,
  });
  const [physicalViewport, setPhysicalViewport] = useState({ scrollTop: 0, height: 600 });
  const activeOwnerKey = useRef(ownerKey);
  useLayoutEffect(() => {
    activeOwnerKey.current = ownerKey;
  }, [ownerKey]);

  const rowOffsets = useMemo(() => {
    const rowCount = rowPlan.length;
    const offsets = new Float64Array(rowCount + 1);
    let nextOffset = 0;
    for (let rowIndex = 0; rowIndex < rowCount; rowIndex++) {
      offsets[rowIndex] = nextOffset;
      nextOffset += typeof rowPlan[rowIndex] === 'number'
        ? measuredHeights.operationRow
        : measuredHeights.groupRow;
    }
    offsets[rowCount] = nextOffset;
    return offsets;
  }, [measuredHeights.groupRow, measuredHeights.operationRow, rowPlan]);
  const liveGeometry = useRef({
    logicalBodyHeight: rowOffsets[rowPlan.length],
    headHeight: measuredHeights.header,
  });
  useLayoutEffect(() => {
    liveGeometry.current = {
      logicalBodyHeight: rowOffsets[rowPlan.length],
      headHeight: measuredHeights.header,
    };
  }, [measuredHeights.header, rowOffsets, rowPlan.length]);

  // Panel layout changes resize the scroll element without resizing the window.
  useLayoutEffect(() => {
    const scrollContainer = wrap;
    if (!scrollContainer) return;
    let animationFrameId: number | null = null;
    let pendingViewport: ResultViewport | null = null;
    let pendingPhysicalViewport: { scrollTop: number; height: number } | null = null;
    const onScroll = () => {
      if (activeOwnerKey.current !== ownerKey) return;
      const scrollTop = scrollContainer.scrollTop;
      const viewportHeight = scrollContainer.clientHeight;
      pendingPhysicalViewport = { scrollTop, height: viewportHeight };
      const geometryInput = liveGeometry.current;
      const projectedGeometry = projectVirtualGeometry({
        logicalBodyHeight: geometryInput.logicalBodyHeight,
        headHeight: geometryInput.headHeight,
        viewportHeight,
        physicalScrollTop: scrollTop,
      });
      pendingViewport = {
        logicalTop: projectedGeometry.logicalScrollTop,
        scrollLeft: scrollContainer.scrollLeft,
      };
      if (animationFrameId !== null) return;
      animationFrameId = requestAnimationFrame(() => {
        animationFrameId = null;
        if (activeOwnerKey.current !== ownerKey
          || pendingViewport === null
          || pendingPhysicalViewport === null) return;
        const nextPhysicalViewport = pendingPhysicalViewport;
        setPhysicalViewport((current) => (
          current.scrollTop === nextPhysicalViewport.scrollTop
            && current.height === nextPhysicalViewport.height
            ? current
            : nextPhysicalViewport
        ));
        onViewportChange(ownerKey, pendingViewport);
        pendingViewport = null;
        pendingPhysicalViewport = null;
      });
    };
    const resizeObserver = new ResizeObserver(() => {
      const height = scrollContainer.clientHeight;
      setPhysicalViewport((current) => (
        current.height === height ? current : { ...current, height }
      ));
    });
    scrollContainer.addEventListener('scroll', onScroll, { passive: true });
    resizeObserver.observe(scrollContainer);
    setPhysicalViewport({
      scrollTop: scrollContainer.scrollTop,
      height: scrollContainer.clientHeight,
    });
    return () => {
      scrollContainer.removeEventListener('scroll', onScroll);
      resizeObserver.disconnect();
      if (animationFrameId !== null) cancelAnimationFrame(animationFrameId);
      if (pendingViewport !== null) onViewportChange(ownerKey, pendingViewport);
    };
  }, [onViewportChange, ownerKey, wrap]);

  useLayoutEffect(() => {
    if (!wrap) return;
    const viewportHeight = wrap.clientHeight;
    const projectedScrollTop = projectLogicalScrollTop({
      logicalBodyHeight: rowOffsets[rowPlan.length],
      headHeight: measuredHeights.header,
      viewportHeight,
      logicalScrollTop: viewport.logicalTop,
    });
    if (Math.abs(wrap.scrollTop - projectedScrollTop) > 0.5) wrap.scrollTop = projectedScrollTop;
    if (Math.abs(wrap.scrollLeft - viewport.scrollLeft) > 0.5) wrap.scrollLeft = viewport.scrollLeft;
    setPhysicalViewport((current) => (
      current.scrollTop === projectedScrollTop && current.height === viewportHeight
        ? current
        : { scrollTop: projectedScrollTop, height: viewportHeight }
    ));
  }, [
    measuredHeights.header,
    ownerKey,
    rowOffsets,
    rowPlan.length,
    viewport.logicalTop,
    viewport.scrollLeft,
    wrap,
  ]);

  // Depending on measuredHeights would let this layout-effect state update trigger itself.
  useLayoutEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    const operationRowElement = body.querySelector('tr:not(.vspacer):not(.grp)') as HTMLElement | null;
    const groupRowElement = body.querySelector('tr.grp') as HTMLElement | null;
    const headerHeight = theadRef.current?.offsetHeight ?? 0;
    setMeasuredHeights((current) => {
      const operationRow = operationRowElement?.offsetHeight || current.operationRow;
      const groupRow = groupRowElement?.offsetHeight || current.groupRow;
      if (Math.abs(operationRow - current.operationRow) < 0.5
        && Math.abs(groupRow - current.groupRow) < 0.5
        && Math.abs(headerHeight - current.header) < 0.5) return current;
      return { operationRow, groupRow, header: headerHeight };
    });
  }, [rowPlan]);

  const rowCount = rowPlan.length;
  if (rowCount === 0) {
    return {
      from: 0,
      to: 0,
      bodyTop: measuredHeights.header,
      canvasHeight: measuredHeights.header,
    };
  }

  const geometry = projectVirtualGeometry({
    logicalBodyHeight: rowOffsets[rowCount],
    headHeight: measuredHeights.header,
    viewportHeight: physicalViewport.height,
    physicalScrollTop: physicalViewport.scrollTop,
  });
  const logicalViewportTop = geometry.logicalBodyTop;
  const from = Math.max(
    0,
    rowIndexAtOffset(rowOffsets, rowCount, logicalViewportTop) - OVERSCAN_ROWS,
  );
  const to = Math.min(
    rowCount,
    rowIndexAtOffset(
      rowOffsets,
      rowCount,
      logicalViewportTop + physicalViewport.height,
    ) + 1 + OVERSCAN_ROWS,
  );

  // Align the selected logical row with its projected physical viewport position. Below the canvas
  // cap this reduces to `header + rowOffsets[from]`; above it, bodyTop stays in physical coordinates.
  const bodyTop = Math.round(
    geometry.physicalScrollTop
      + measuredHeights.header
      + rowOffsets[from]
      - geometry.logicalScrollTop,
  );
  return { from, to, bodyTop, canvasHeight: geometry.canvasHeight };
}
