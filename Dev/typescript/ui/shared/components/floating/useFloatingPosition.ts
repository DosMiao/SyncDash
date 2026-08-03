import { useLayoutEffect, useRef, type RefObject } from 'react';
import { clampFloatingPanel } from './geometry.ts';

/// The unclamped placement a panel asks for, plus the size to clamp it against. Callers measure the
/// panel themselves because a panel that has not laid out yet reports zero and each one has its own
/// sensible fallback extent.
export interface FloatingPlacement {
  left: number;
  top: number;
  width: number;
  height: number;
}

interface FloatingPositionOptions {
  /// Skipped entirely while false, so a closed panel installs no listeners.
  enabled?: boolean;
  /// Panels anchored to a scrollable element must follow it; a panel anchored to a fixed point
  /// (or one that closes on scroll) must not.
  repositionOnScroll?: boolean;
  /// Re-clamp when the panel's own content changes its height.
  observeSelf?: boolean;
  /// What `measure` reads besides the panel element itself.
  deps: readonly unknown[];
}

/**
 * Clamp a floating panel into the webview and publish the result as `--floating-panel-top/left`.
 *
 * The Tauri CSP blocks `style=""`, so the position reaches the stylesheet through the CSSOM; the
 * properties default to an off-screen value so the panel is never painted at its unclamped
 * placement before the first layout measurement.
 */
export function useFloatingPosition<E extends HTMLElement>(
  panelRef: RefObject<E | null>,
  measure: (panel: E) => FloatingPlacement | null,
  options: FloatingPositionOptions,
): void {
  const { enabled = true, repositionOnScroll = false, observeSelf = false, deps } = options;
  const latestMeasure = useRef(measure);
  latestMeasure.current = measure;

  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (!enabled || !panel) return;
    const updatePosition = () => {
      const placement = latestMeasure.current(panel);
      if (!placement) return;
      const position = clampFloatingPanel(
        placement.left,
        placement.top,
        placement.width,
        placement.height,
      );
      panel.style.setProperty('--floating-panel-top', `${position.top}px`);
      panel.style.setProperty('--floating-panel-left', `${position.left}px`);
    };
    updatePosition();
    window.addEventListener('resize', updatePosition);
    if (repositionOnScroll) document.addEventListener('scroll', updatePosition, true);
    const resizeObserver = observeSelf ? new ResizeObserver(updatePosition) : null;
    resizeObserver?.observe(panel);
    return () => {
      resizeObserver?.disconnect();
      window.removeEventListener('resize', updatePosition);
      if (repositionOnScroll) document.removeEventListener('scroll', updatePosition, true);
      panel.style.removeProperty('--floating-panel-top');
      panel.style.removeProperty('--floating-panel-left');
    };
  }, [panelRef, enabled, repositionOnScroll, observeSelf, ...deps]);
}
