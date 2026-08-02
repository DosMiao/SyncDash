import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

import { formToSettings, settingsToForm } from '../../typescript/core/formSchema.ts';
import type { AppSettings } from '../../typescript/core/types/generated/AppSettings.ts';
import type { SettingsNumericLimitsDto } from '../../typescript/core/types/generated/SettingsNumericLimitsDto.ts';

const limits: SettingsNumericLimitsDto = {
  maximum_keep_days: 36_500,
  maximum_total_mb: 1_048_576,
};

const settings: AppSettings = {
  log_dir: '',
  level: 'info',
  keep_days: 30,
  max_total_mb: 512,
  log_compare: 'summary',
  mirror_stderr: true,
};

test('settings form accepts only bounded whole-number retention values', () => {
  const values = settingsToForm(settings);
  assert.deepEqual(formToSettings(values, limits), { settings });

  assert.deepEqual(formToSettings({ ...values, keep_days: '-1' }, limits), {
    error: 'Retention days must be a whole number from 0 through 36,500',
    field: 'keep_days',
  });
  assert.deepEqual(formToSettings({ ...values, max_total_mb: '1.5' }, limits), {
    error: 'Total size cap in MB must be a whole number from 0 through 1,048,576',
    field: 'max_total_mb',
  });
  assert.deepEqual(formToSettings({ ...values, keep_days: '36501' }, limits), {
    error: 'Retention days must be a whole number from 0 through 36,500',
    field: 'keep_days',
  });
});

test('log directory mutation crosses only the native grant boundary', async () => {
  const [command, ipc, sheet] = await Promise.all([
    readFile(new URL('../../src-tauri/src/cmd/logs.rs', import.meta.url), 'utf8'),
    readFile(new URL('../../typescript/core/ipc.ts', import.meta.url), 'utf8'),
    readFile(new URL('../../typescript/ui/components/SettingsSheet.tsx', import.meta.url), 'utf8'),
  ]);
  assert.match(command, /pick_log_directory[\s\S]*?\.pick_folder\(/);
  assert.match(command, /consume_log_directory_grant\(/);
  assert.match(command, /save_if_revision\(&settings, &expected_revision\)/);
  assert.doesNotMatch(command, /\bmigrate:\s*bool\b/);
  assert.match(ipc, /invoke<LogDirectorySelectionDto \| null>\('pick_log_directory'/);
  assert.match(sheet, /pickLogDirectory\(settingsSnapshot\.revision\)/);
  assert.doesNotMatch(sheet, /\bpickDirectory\b/);
  assert.match(sheet, /readOnlyField=\{\(key\) => key === 'log_dir'\}/);
  assert.match(sheet, /activeMutation\.current/);
  assert.match(sheet, /componentMounted\.current = true;[\s\S]*componentMounted\.current = false;/);
});

test('log reading and reveal use typed purpose-specific commands with server limits', async () => {
  const [command, ipc] = await Promise.all([
    readFile(new URL('../../src-tauri/src/cmd/logs.rs', import.meta.url), 'utf8'),
    readFile(new URL('../../typescript/core/ipc.ts', import.meta.url), 'utf8'),
  ]);
  assert.match(command, /artifact:\s*syncdash::obs::runlog::LogArtifactKind/);
  assert.match(command, /LOG_ARTIFACT_LINE_LIMIT/);
  assert.doesNotMatch(command, /\bmax:\s*Option<usize>/);
  assert.doesNotMatch(command, /\bwhich:\s*String/);
  assert.match(ipc, /invoke<void>\('reveal_log_location'/);
  assert.doesNotMatch(ipc, /\blogDirPath\b/);
});
