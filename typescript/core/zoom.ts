// UI scale.
//
// This scales the **webview**, not the stylesheet: setZoom is the same mechanism as Ctrl +/- in a browser,
// so borders, images, the virtual scroller's row heights and the fixed table columns all scale together.
// Scaling type alone (a root font-size knob with rem everywhere) would leave every px dimension behind and
// the layout would come apart at the edges of the range.
//
// Both windows read the same key, so the progress sub-window opens at the size the main window is using.

import { getCurrentWebview } from '@tauri-apps/api/webview';

const KEY = 'sd.zoom';

/// Discrete steps, the way browsers do it — a continuous slider lands on factors that make text blurry
export const ZOOM_STEPS = [0.8, 0.9, 1, 1.1, 1.25, 1.4, 1.6, 1.8, 2] as const;

export function readZoom(): number {
  const v = Number(localStorage.getItem(KEY));
  return Number.isFinite(v) && v > 0 ? v : 1;
}

/// Applies and persists. Failure is not fatal: in a plain browser (vite dev) there is no webview to zoom,
/// and the browser's own Ctrl +/- still works.
export async function applyZoom(factor: number): Promise<number> {
  const f = Math.min(ZOOM_STEPS[ZOOM_STEPS.length - 1], Math.max(ZOOM_STEPS[0], factor));
  localStorage.setItem(KEY, String(f));
  try { await getCurrentWebview().setZoom(f); } catch { /* not running under Tauri */ }
  return f;
}

export function stepZoom(current: number, dir: 1 | -1): number {
  const i = ZOOM_STEPS.findIndex((z) => Math.abs(z - current) < 0.001);
  if (i < 0) return 1;
  return ZOOM_STEPS[Math.min(ZOOM_STEPS.length - 1, Math.max(0, i + dir))];
}
