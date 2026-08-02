import { invoke } from '@tauri-apps/api/core';

import type { JunkPresetDto } from '#core/types/generated/JunkPresetDto.ts';
import type { PathVerdict } from '#core/types/generated/PathVerdict.ts';

export type { JunkPresetDto };

export const inspectPaths = (source: string, target: string) =>
  invoke<PathVerdict>('inspect_paths', { source, target });

// Presets are fetched because their exact patterns are engine policy.
export const junkPresets = () => invoke<JunkPresetDto[]>('junk_presets');
