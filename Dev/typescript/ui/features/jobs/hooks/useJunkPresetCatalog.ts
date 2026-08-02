import { useEffect, useState } from 'react';
import { junkPresets } from '#core/infrastructure/tauri/commands/paths.ts';
import type { JunkPresetDto } from '#core/types/generated/JunkPresetDto.ts';

export type JunkPresetCatalog =
  | { status: 'loading'; presets: JunkPresetDto[] }
  | { status: 'ready'; presets: JunkPresetDto[] }
  | { status: 'failed'; presets: JunkPresetDto[]; error: string };

export function useJunkPresetCatalog(): JunkPresetCatalog {
  const [catalog, setCatalog] = useState<JunkPresetCatalog>({ status: 'loading', presets: [] });
  useEffect(() => {
    let live = true;
    junkPresets().then(
      (presets) => {
        if (live) setCatalog({ status: 'ready', presets });
      },
      (error: unknown) => {
        if (live) setCatalog({ status: 'failed', presets: [], error: String(error) });
      },
    );
    return () => { live = false; };
  }, []);
  return catalog;
}
