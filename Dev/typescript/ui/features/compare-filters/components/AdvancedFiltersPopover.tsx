import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { clampFloatingPanel } from '#ui/shared/components/floating/geometry.ts';
import { useInteractionLayer } from '#ui/shared/interaction/useInteractionLayer.tsx';
import {
  advancedFiltersEqual,
  createAdvancedFilterDraft,
  createEmptyAdvancedFilterDraft,
  validateAdvancedFilterDraft,
} from '#core/application/compare-workspace/compareWorkspaceFilters.ts';
import type { AdvancedScopeFilter } from '#core/domain/compare/runScope.ts';

interface AdvancedFiltersPopoverProps {
  anchor: DOMRect;
  appliedFilter: AdvancedScopeFilter;
  inScopeCount: number;
  differenceCount: number;
  onApplyFilter: (filter: AdvancedScopeFilter) => void;
  onWriteValidatedFilterMasksToJobExclude: (filter: AdvancedScopeFilter) => void;
  onDismiss: () => void;
}

const MODIFIED_RANGES: [string, number | null][] = [
  ['Any Time', null],
  ['Last 24 Hours', 1],
  ['Last 7 Days', 7],
  ['Last 30 Days', 30],
];

export function AdvancedFiltersPopover(props: AdvancedFiltersPopoverProps) {
  const {
    anchor,
    appliedFilter,
    inScopeCount,
    differenceCount,
    onApplyFilter,
    onWriteValidatedFilterMasksToJobExclude,
    onDismiss,
  } = props;
  const popoverRef = useRef<HTMLDivElement>(null);
  const previousFocus = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const [draft, setDraft] = useState(() => createAdvancedFilterDraft(appliedFilter));
  const validationMessageId = useId();
  const appliedScopeStatusId = useId();
  const latestOnDismiss = useRef(onDismiss);
  latestOnDismiss.current = onDismiss;

  const restorePreviousFocus = useCallback(() => {
    const previous = previousFocus.current;
    if (previous?.isConnected) requestAnimationFrame(() => previous.focus());
  }, []);

  const dismiss = useCallback((restoreFocus: boolean) => {
    latestOnDismiss.current();
    if (restoreFocus) restorePreviousFocus();
  }, [restorePreviousFocus]);

  useInteractionLayer({
    kind: 'popover',
    rootRef: popoverRef,
    handlers: { dismiss: () => dismiss(true) },
  });

  // Both axes are clamped because opening this tall panel near the bottom of a short window would
  // otherwise leave its action buttons unreachable.
  useLayoutEffect(() => {
    const element = popoverRef.current;
    if (!element) return;
    const updatePosition = () => {
    const position = clampFloatingPanel(
        anchor.left,
        anchor.bottom + 4,
        element.offsetWidth || 400,
        element.offsetHeight || 380,
      );
      element.style.setProperty('--advanced-filters-left', `${position.left}px`);
      element.style.setProperty('--advanced-filters-top', `${position.top}px`);
    };
    updatePosition();
    window.addEventListener('resize', updatePosition);
    document.addEventListener('scroll', updatePosition, true);
    const resizeObserver = new ResizeObserver(updatePosition);
    resizeObserver.observe(element);
    return () => {
      resizeObserver.disconnect();
      window.removeEventListener('resize', updatePosition);
      document.removeEventListener('scroll', updatePosition, true);
      element.style.removeProperty('--advanced-filters-left');
      element.style.removeProperty('--advanced-filters-top');
    };
  }, [anchor]);

  useLayoutEffect(() => {
    popoverRef.current?.querySelector<HTMLElement>('textarea, input, button')?.focus();
  }, []);

  useEffect(() => {
    const onDocumentClick = (event: MouseEvent) => {
      if (popoverRef.current?.contains(event.target as Node)) return;
      dismiss(false);
    };
    document.addEventListener('click', onDocumentClick);
    return () => {
      document.removeEventListener('click', onDocumentClick);
    };
  }, [dismiss]);

  const validation = validateAdvancedFilterDraft(draft);
  const validationMessage = validation.status === 'invalid' ? validation.messages.join(' ') : null;
  const validatedFilter = validation.status === 'valid' ? validation.filter : null;
  const draftMatchesAppliedFilter = validatedFilter !== null
    && advancedFiltersEqual(validatedFilter, appliedFilter);
  const outsideScopeCount = differenceCount - inScopeCount;

  return (
    <div
      className="advanced-filters-popover"
      ref={popoverRef}
      role="dialog"
      aria-label="Advanced Filters"
      aria-describedby={validationMessage
        ? `${validationMessageId} ${appliedScopeStatusId}`
        : appliedScopeStatusId}
      onClick={(event) => event.stopPropagation()}
    >
      <div className="advanced-filters-head">
        Advanced Filters<span className="hint">Current Result · No Rescan</span>
      </div>
      <label className="advanced-filters-row wide">
        <span>Name Masks (FFS Syntax, One per Line)</span>
        <textarea
          className="mono"
          spellCheck={false}
          placeholder={'*/*.log\n/Course/0 Mizzou Courses/.git/'}
          value={draft.nameMasksText}
          onChange={(event) => setDraft((current) => ({
            ...current,
            nameMasksText: event.target.value,
          }))}
        />
      </label>
      <label className="advanced-filters-row">
        <span>Size (MiB)</span>
        <span className="advanced-filters-pair">
          <input
            type="number" step="any" min="0" placeholder="≥"
            aria-label="Minimum Size in Mebibytes"
            aria-invalid={Boolean(validation.status === 'invalid' && validation.errors.minimumSizeMiBText)}
            aria-describedby={validation.status === 'invalid' && validation.errors.minimumSizeMiBText
              ? validationMessageId
              : undefined}
            value={draft.minimumSizeMiBText}
            onChange={(event) => setDraft((current) => ({
              ...current,
              minimumSizeMiBText: event.target.value,
            }))}
          />
          <input
            type="number" step="any" min="0" placeholder="≤"
            aria-label="Maximum Size in Mebibytes"
            aria-invalid={Boolean(validation.status === 'invalid' && validation.errors.maximumSizeMiBText)}
            aria-describedby={validation.status === 'invalid' && validation.errors.maximumSizeMiBText
              ? validationMessageId
              : undefined}
            value={draft.maximumSizeMiBText}
            onChange={(event) => setDraft((current) => ({
              ...current,
              maximumSizeMiBText: event.target.value,
            }))}
          />
        </span>
      </label>
      <div className="advanced-filters-row" role="group" aria-label="Modified Date">
        <span>Modified</span>
        <div className="advanced-filters-presets">
          {MODIFIED_RANGES.map(([label, days]) => (
            <button
              type="button"
              key={label}
              className={'chip' + (draft.modifiedWithinDays === days ? ' on' : '')}
              aria-pressed={draft.modifiedWithinDays === days}
              onClick={() => setDraft((current) => ({ ...current, modifiedWithinDays: days }))}
            >{label}</button>
          ))}
        </div>
      </div>
      {validationMessage && (
        <div
          className="advanced-filters-stat advanced-filters-validation"
          id={validationMessageId}
          role="status"
          aria-live="polite"
        >
          Draft not applied: {validationMessage}
        </div>
      )}
      <div
        className="advanced-filters-stat dim"
        id={appliedScopeStatusId}
        role="status"
        aria-live="polite"
      >
        {outsideScopeCount > 0
          ? `Applied scope: ${inScopeCount} of ${differenceCount}; ${outsideScopeCount} will not run`
          : `Applied scope: all ${differenceCount} differences`}
        {draftMatchesAppliedFilter
          ? '. Draft matches the applied filter.'
          : '. Draft changes are not active.'}
      </div>
      <div className="advanced-filters-buttons">
        <button
          type="button"
          className="btn"
          title="Reset this draft. The applied run scope does not change until you apply it."
          onClick={() => setDraft(createEmptyAdvancedFilterDraft())}
        >Clear Draft</button>
        <button
          type="button"
          className="btn"
          title={validationMessage
            ?? (validatedFilter?.masks.length === 0
              ? 'Enter at least one valid name mask first.'
              : "Apply this draft and add its name masks to the job's exclude list.")}
          disabled={!validatedFilter || validatedFilter.masks.length === 0}
          onClick={() => {
            if (!validatedFilter) return;
            onWriteValidatedFilterMasksToJobExclude(validatedFilter);
            dismiss(true);
          }}
        >Write to Job Exclude</button>
        <button
          type="button"
          className="btn"
          onClick={() => dismiss(true)}
        >Cancel</button>
        <button
          type="button"
          className="btn accent"
          title={validationMessage ?? 'Apply this draft to the current result.'}
          disabled={!validatedFilter}
          onClick={() => {
            if (!validatedFilter) return;
            onApplyFilter(validatedFilter);
            dismiss(true);
          }}
        >Apply</button>
      </div>
    </div>
  );
}
