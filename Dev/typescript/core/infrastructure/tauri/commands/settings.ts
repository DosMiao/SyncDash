import { invoke } from '@tauri-apps/api/core';

import type { AppSettings } from '#core/types/generated/AppSettings.ts';
import type { LogDirectorySelectionDto } from '#core/types/generated/LogDirectorySelectionDto.ts';
import type { SettingsSaveDto } from '#core/types/generated/SettingsSaveDto.ts';
import type { SettingsSnapshotDto } from '#core/types/generated/SettingsSnapshotDto.ts';

export const getSettings = () => invoke<SettingsSnapshotDto>('get_settings');
export const pickLogDirectory = (expectedRevision: string) =>
  invoke<LogDirectorySelectionDto | null>('pick_log_directory', { expectedRevision });
export const saveSettings = (
  settings: AppSettings,
  expectedRevision: string,
  logDirectoryGrant?: string,
) => invoke<SettingsSaveDto>('save_settings', { settings, expectedRevision, logDirectoryGrant });
