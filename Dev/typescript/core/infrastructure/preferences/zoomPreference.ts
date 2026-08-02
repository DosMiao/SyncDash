import {
  isZoomFactor,
  requireZoomFactor,
} from '#core/application/zoom/zoomAuthority.ts';
import type {
  ZoomPreferenceLoad,
} from '#core/application/zoom/zoomAuthority.ts';

const ZOOM_PREFERENCE_KEY = 'sd.zoom';

interface StorageReader {
  getItem(key: string): string | null;
}

interface StorageWriter {
  setItem(key: string, value: string): void;
}

export function loadZoomPreference(
  storage: StorageReader = localStorage,
): ZoomPreferenceLoad {
  let storedFactor: string | null;
  try {
    storedFactor = storage.getItem(ZOOM_PREFERENCE_KEY);
  } catch (error) {
    return {
      factor: 1,
      persistedFactor: null,
      warning: `Could not read the interface zoom preference: ${String(error)}`,
    };
  }
  if (storedFactor === null) {
    return { factor: 1, persistedFactor: null, warning: null };
  }
  const parsedFactor = Number(storedFactor);
  if (isZoomFactor(parsedFactor)) {
    return { factor: parsedFactor, persistedFactor: parsedFactor, warning: null };
  }
  return {
    factor: 1,
    persistedFactor: null,
    warning: `Ignored invalid interface zoom preference: ${storedFactor}`,
  };
}

export function saveZoomPreference(
  factor: number,
  storage: StorageWriter = localStorage,
): string | null {
  const validatedFactor = requireZoomFactor(factor);
  try {
    storage.setItem(ZOOM_PREFERENCE_KEY, String(validatedFactor));
    return null;
  } catch (error) {
    return String(error);
  }
}
