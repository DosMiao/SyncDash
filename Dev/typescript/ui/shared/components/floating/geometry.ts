/// Keep a floating panel visibly separated from every edge of the webview.
export function clampFloatingPanel(
  left: number,
  top: number,
  width: number,
  height: number,
): { left: number; top: number } {
  return {
    left: Math.max(6, Math.min(left, window.innerWidth - width - 6)),
    top: Math.max(6, Math.min(top, window.innerHeight - height - 6)),
  };
}
