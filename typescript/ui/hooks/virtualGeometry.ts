// Geometry for the virtual table.
//
// The logical list can be millions of pixels tall, but that number must never become a CSS height:
// doing so only virtualizes the DOM node count while leaving WebKit to scroll, tile and repaint one
// enormous coordinate space. Keep the browser-facing canvas bounded and map its scroll range onto
// the complete logical range instead.

export const MAX_PHYSICAL_BODY_PX = 1_500_000;

export interface VirtualGeometryInput {
  logicalBodyHeight: number;
  headHeight: number;
  viewportHeight: number;
  scrollTop: number;
}

export interface VirtualGeometry {
  /// Height assigned to the real DOM canvas.
  canvasHeight: number;
  /// Browser scrollTop, clamped to the real canvas range (rubber-band scrolling can report < 0).
  physicalScroll: number;
  /// Equivalent position in the complete logical list, including the header.
  logicalScroll: number;
  /// Logical body coordinate at the top of the viewport.
  logicalBodyTop: number;
}

export function projectVirtualGeometry(input: VirtualGeometryInput): VirtualGeometry {
  const logicalBodyHeight = Math.max(0, input.logicalBodyHeight);
  const headHeight = Math.max(0, input.headHeight);
  const viewportHeight = Math.max(0, input.viewportHeight);
  const physicalBodyHeight = Math.min(logicalBodyHeight, MAX_PHYSICAL_BODY_PX);
  const canvasHeight = headHeight + physicalBodyHeight;

  const physicalRange = Math.max(0, canvasHeight - viewportHeight);
  const logicalRange = Math.max(0, headHeight + logicalBodyHeight - viewportHeight);
  const physicalScroll = Math.min(physicalRange, Math.max(0, input.scrollTop));
  const logicalScroll = physicalRange > 0
    ? physicalScroll * (logicalRange / physicalRange)
    : 0;

  return {
    canvasHeight,
    physicalScroll,
    logicalScroll,
    logicalBodyTop: Math.max(0, logicalScroll - headHeight),
  };
}
