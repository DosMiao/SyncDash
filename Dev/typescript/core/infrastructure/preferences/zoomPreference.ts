import {
  isZoomFactor,
  requireZoomFactor,
} from '#core/application/zoom/zoomAuthority.ts';
import type {
  ZoomPreferenceLoad,
} from '#core/application/zoom/zoomAuthority.ts';
import {
  preferenceErrorMessage,
  type PreferenceStorageReader,
  type PreferenceStorageWriter,
} from './preferenceStorage.ts';

const ZOOM_PREFERENCE_KEY = 'sd.zoom';

export function loadZoomPreference(
  storage: PreferenceStorageReader = localStorage,
): ZoomPreferenceLoad {
  let storedFactor: string | null;
  try {
    storedFactor = storage.getItem(ZOOM_PREFERENCE_KEY);
  } catch (error) {
    return {
      factor: 1,
      persistedFactor: null,
      warning: `Could not read the interface zoom preference: ${preferenceErrorMessage(error)}`,
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
  storage: PreferenceStorageWriter = localStorage,
): string | null {
  const validatedFactor = requireZoomFactor(factor);
  try {
    storage.setItem(ZOOM_PREFERENCE_KEY, String(validatedFactor));
    return null;
  } catch (error) {
    return preferenceErrorMessage(error);
  }
}
