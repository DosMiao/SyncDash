import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';

import { requireZoomFactor } from '#core/application/zoom/zoomAuthority.ts';
import type { ZoomFactor } from '#core/application/zoom/zoomAuthority.ts';

export async function applyZoom(factor: number): Promise<ZoomFactor> {
  const validatedFactor = requireZoomFactor(factor);
  if (isTauri()) await getCurrentWebview().setZoom(validatedFactor);
  return validatedFactor;
}
