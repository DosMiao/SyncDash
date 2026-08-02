// Geometry for the virtual table.
//
// The logical list can be millions of pixels tall, but that number must never become a CSS height:
// doing so only virtualizes the DOM node count while leaving WebKit to scroll, tile and repaint one
// enormous coordinate space. Keep the browser-facing canvas bounded and map its scroll range onto
// the complete logical range instead.

export const maximumPhysicalBodyPixels = 1_500_000;

export interface VirtualGeometryInput {
  logicalBodyHeight: number;
  headHeight: number;
  viewportHeight: number;
  physicalScrollTop: number;
}

export interface VirtualGeometry {
  /// Height assigned to the real DOM canvas.
  canvasHeight: number;
  /// Browser scrollTop, clamped to the real canvas range (rubber-band scrolling can report < 0).
  physicalScrollTop: number;
  /// Equivalent position in the complete logical list, including the header.
  logicalScrollTop: number;
  /// Logical body coordinate at the top of the viewport.
  logicalBodyTop: number;
}

export function projectVirtualGeometry(input: VirtualGeometryInput): VirtualGeometry {
  const logicalBodyHeight = Math.max(0, input.logicalBodyHeight);
  const headHeight = Math.max(0, input.headHeight);
  const viewportHeight = Math.max(0, input.viewportHeight);
  const physicalBodyHeight = Math.min(logicalBodyHeight, maximumPhysicalBodyPixels);
  const canvasHeight = headHeight + physicalBodyHeight;

  const physicalRange = Math.max(0, canvasHeight - viewportHeight);
  const logicalRange = Math.max(0, headHeight + logicalBodyHeight - viewportHeight);
  const physicalScrollTop = Math.min(physicalRange, Math.max(0, input.physicalScrollTop));
  const logicalScrollTop = physicalRange > 0
    ? physicalScrollTop * (logicalRange / physicalRange)
    : 0;

  return {
    canvasHeight,
    physicalScrollTop,
    logicalScrollTop,
    logicalBodyTop: Math.max(0, logicalScrollTop - headHeight),
  };
}

export function projectLogicalScrollTop(input: {
  logicalBodyHeight: number;
  headHeight: number;
  viewportHeight: number;
  logicalScrollTop: number;
}): number {
  const logicalBodyHeight = Math.max(0, input.logicalBodyHeight);
  const headHeight = Math.max(0, input.headHeight);
  const viewportHeight = Math.max(0, input.viewportHeight);
  const physicalBodyHeight = Math.min(logicalBodyHeight, maximumPhysicalBodyPixels);
  const physicalRange = Math.max(0, headHeight + physicalBodyHeight - viewportHeight);
  const logicalRange = Math.max(0, headHeight + logicalBodyHeight - viewportHeight);
  if (logicalRange === 0) return 0;
  const logicalScrollTop = Math.min(logicalRange, Math.max(0, input.logicalScrollTop));
  return logicalScrollTop * (physicalRange / logicalRange);
}
