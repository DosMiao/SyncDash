import assert from 'node:assert/strict';
import test from 'node:test';

import { formToSettings, settingsToForm } from '#core/domain/jobs/formSchema.ts';
import type { AppSettings } from '#core/types/generated/AppSettings.ts';
import type { SettingsNumericLimitsDto } from '#core/types/generated/SettingsNumericLimitsDto.ts';

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
