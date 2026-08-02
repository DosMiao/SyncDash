import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

import {
  createAdvancedFilterDraft,
  createEmptyAdvancedFilterDraft,
  validateAdvancedFilterDraft,
  validatedAdvancedFilter,
} from '../../typescript/ui/state/compareWorkspaceFilters.ts';

test('advanced-filter drafts round-trip applied criteria without retaining editable text', () => {
  assert.deepEqual(createAdvancedFilterDraft({
    masks: ['*/*.log', '/cache/'],
    minimumMiB: 1.5,
    maximumMiB: 20,
    modifiedWithinDays: 7,
  }), {
    nameMasksText: '*/*.log\n/cache/',
    minimumSizeMiBText: '1.5',
    maximumSizeMiBText: '20',
    modifiedWithinDays: 7,
  });
  assert.deepEqual(createEmptyAdvancedFilterDraft(), {
    nameMasksText: '',
    minimumSizeMiBText: '',
    maximumSizeMiBText: '',
    modifiedWithinDays: null,
  });
});

test('advanced-filter validation rejects intermediate, negative, and inverted size input', () => {
  const intermediate = validateAdvancedFilterDraft({
    ...createEmptyAdvancedFilterDraft(),
    minimumSizeMiBText: '1e',
  });
  assert.equal(intermediate.status, 'invalid');
  if (intermediate.status === 'invalid') {
    assert.match(intermediate.errors.minimumSizeMiBText ?? '', /finite number/);
  }

  const negative = validateAdvancedFilterDraft({
    ...createEmptyAdvancedFilterDraft(),
    maximumSizeMiBText: '-1',
  });
  assert.equal(negative.status, 'invalid');
  if (negative.status === 'invalid') {
    assert.match(negative.errors.maximumSizeMiBText ?? '', /cannot be negative/);
  }

  const inverted = validateAdvancedFilterDraft({
    ...createEmptyAdvancedFilterDraft(),
    minimumSizeMiBText: '10',
    maximumSizeMiBText: '2',
  });
  assert.equal(inverted.status, 'invalid');
  if (inverted.status === 'invalid') {
    assert.match(inverted.errors.maximumSizeMiBText ?? '', /greater than or equal/);
  }
});

test('advanced-filter validation publishes one canonical filter', () => {
  const validation = validateAdvancedFilterDraft({
    nameMasksText: '  */*.log  \n\n /cache/ ',
    minimumSizeMiBText: '0',
    maximumSizeMiBText: '12.25',
    modifiedWithinDays: 30,
  });
  assert.deepEqual(validation, {
    status: 'valid',
    filter: {
      masks: ['*/*.log', '/cache/'],
      minimumMiB: 0,
      maximumMiB: 12.25,
      modifiedWithinDays: 30,
    },
  });
  assert.throws(
    () => validatedAdvancedFilter({
      masks: [],
      minimumMiB: 8,
      maximumMiB: 4,
      modifiedWithinDays: null,
    }),
    /Advanced filter is invalid/,
  );
});

test('advanced-filter popover exposes explicit apply and discard actions', async () => {
  const source = await readFile(
    new URL('../../typescript/ui/components/AdvancedFiltersPopover.tsx', import.meta.url),
    'utf8',
  );
  assert.match(source, /useState\(\(\) => createAdvancedFilterDraft\(appliedFilter\)\)/);
  assert.doesNotMatch(source, /maskDraft|onMaskDraftChange|onAdvancedFilterChange/);
  assert.match(source, />Clear Draft<\/button>/);
  assert.match(source, />Cancel<\/button>/);
  assert.match(source, />Apply<\/button>/);
  assert.match(source, /onApplyFilter\(validatedFilter\);\s*dismiss\(true\);/);
  assert.match(source, /onWriteValidatedFilterMasksToJobExclude\(validatedFilter\);\s*dismiss\(true\);/);
  assert.match(source, /handlers: \{ dismiss: \(\) => dismiss\(true\) \}/);
  assert.match(source, /if \(popoverRef\.current\?\.contains\(event\.target as Node\)\) return;\s*dismiss\(false\);/);
});
