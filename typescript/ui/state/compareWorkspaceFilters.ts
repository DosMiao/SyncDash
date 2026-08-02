// Advanced-filter editing is a transaction: raw control text stays outside retained workspace
// state, and this module is the only boundary that converts it into executable scope criteria.

import { EMPTY_ADVANCED_SCOPE_FILTER, parseScopeMasks } from '../../core/runScope.ts';
import type { AdvancedScopeFilter } from '../../core/runScope.ts';

export interface AdvancedFilterDraft {
  nameMasksText: string;
  minimumSizeMiBText: string;
  maximumSizeMiBText: string;
  modifiedWithinDays: number | null;
}

export interface AdvancedFilterDraftErrors {
  minimumSizeMiBText?: string;
  maximumSizeMiBText?: string;
  modifiedWithinDays?: string;
}

export type AdvancedFilterDraftValidation =
  | { status: 'valid'; filter: AdvancedScopeFilter }
  | { status: 'invalid'; errors: AdvancedFilterDraftErrors; messages: string[] };

interface ParsedSize {
  value: number | null;
  error?: string;
}

function parseOptionalSize(text: string, label: string): ParsedSize {
  const trimmed = text.trim();
  if (trimmed === '') return { value: null };
  const value = Number(trimmed);
  if (!Number.isFinite(value)) return { value: null, error: `${label} must be a finite number.` };
  if (value < 0) return { value: null, error: `${label} cannot be negative.` };
  return { value: value === 0 ? 0 : value };
}

export function advancedFilterMasksEqual(
  left: AdvancedScopeFilter,
  right: AdvancedScopeFilter,
): boolean {
  return left.masks.length === right.masks.length
    && left.masks.every((mask, index) => mask === right.masks[index]);
}

export function advancedFiltersEqual(
  left: AdvancedScopeFilter,
  right: AdvancedScopeFilter,
): boolean {
  return advancedFilterMasksEqual(left, right)
    && left.minimumMiB === right.minimumMiB
    && left.maximumMiB === right.maximumMiB
    && left.modifiedWithinDays === right.modifiedWithinDays;
}

export function createAdvancedFilterDraft(filter: AdvancedScopeFilter): AdvancedFilterDraft {
  return {
    nameMasksText: filter.masks.join('\n'),
    minimumSizeMiBText: filter.minimumMiB === null ? '' : String(filter.minimumMiB),
    maximumSizeMiBText: filter.maximumMiB === null ? '' : String(filter.maximumMiB),
    modifiedWithinDays: filter.modifiedWithinDays,
  };
}

export function createEmptyAdvancedFilterDraft(): AdvancedFilterDraft {
  return createAdvancedFilterDraft(EMPTY_ADVANCED_SCOPE_FILTER);
}

export function validateAdvancedFilterDraft(
  draft: AdvancedFilterDraft,
): AdvancedFilterDraftValidation {
  const minimumSize = parseOptionalSize(draft.minimumSizeMiBText, 'Minimum size');
  const maximumSize = parseOptionalSize(draft.maximumSizeMiBText, 'Maximum size');
  const errors: AdvancedFilterDraftErrors = {};
  if (minimumSize.error) errors.minimumSizeMiBText = minimumSize.error;
  if (maximumSize.error) errors.maximumSizeMiBText = maximumSize.error;
  if (draft.modifiedWithinDays !== null
    && (!Number.isFinite(draft.modifiedWithinDays) || draft.modifiedWithinDays < 0)
  ) {
    errors.modifiedWithinDays = 'Modified date range must be a non-negative number of days.';
  }
  if (!minimumSize.error
    && !maximumSize.error
    && minimumSize.value !== null
    && maximumSize.value !== null
    && minimumSize.value > maximumSize.value
  ) {
    errors.maximumSizeMiBText = 'Maximum size must be greater than or equal to minimum size.';
  }

  const messages = Object.values(errors);
  if (messages.length > 0) return { status: 'invalid', errors, messages };
  return {
    status: 'valid',
    filter: {
      masks: parseScopeMasks(draft.nameMasksText),
      minimumMiB: minimumSize.value,
      maximumMiB: maximumSize.value,
      modifiedWithinDays: draft.modifiedWithinDays,
    },
  };
}

export function validatedAdvancedFilter(filter: AdvancedScopeFilter): AdvancedScopeFilter {
  const validation = validateAdvancedFilterDraft(createAdvancedFilterDraft(filter));
  if (validation.status === 'invalid') {
    throw new Error(`Advanced filter is invalid: ${validation.messages.join(' ')}`);
  }
  return validation.filter;
}
