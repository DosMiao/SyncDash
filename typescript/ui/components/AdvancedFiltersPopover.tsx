import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { clamp } from './ui';
import { parseScopeMasks } from '../../core/runScope';
import type { AdvancedScopeFilter } from '../../core/runScope';

interface AdvancedFiltersPopoverProps {
  anchor: DOMRect;
  advancedFilter: AdvancedScopeFilter;
  maskDraft: string;
  inScopeCount: number;
  differenceCount: number;
  onAdvancedFilterChange: (next: AdvancedScopeFilter) => void;
  onMaskDraftChange: (next: string) => void;
  onClear: () => void;
  onWriteMasksToJob: (masks: string[]) => void;
  onClose: () => void;
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
    advancedFilter,
    maskDraft,
    inScopeCount,
    differenceCount,
    onAdvancedFilterChange,
    onMaskDraftChange,
    onClear,
    onWriteMasksToJob,
    onClose,
  } = props;
  const popoverRef = useRef<HTMLDivElement>(null);
  const previousFocus = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const [position, setPosition] = useState({ left: anchor.left, top: anchor.bottom + 4 });
  const latestOnClose = useRef(onClose);
  latestOnClose.current = onClose;

  const close = (restoreFocus: boolean) => {
    latestOnClose.current();
    const previous = previousFocus.current;
    if (restoreFocus && previous?.isConnected) requestAnimationFrame(() => previous.focus());
  };

  // Both axes are clamped because opening this tall panel near the bottom of a short window would
  // otherwise leave its action buttons unreachable.
  useLayoutEffect(() => {
    const element = popoverRef.current;
    setPosition(clamp(
      anchor.left,
      anchor.bottom + 4,
      element?.offsetWidth ?? 400,
      element?.offsetHeight ?? 380,
    ));
  }, [anchor]);

  useLayoutEffect(() => {
    popoverRef.current?.querySelector<HTMLElement>('textarea, input, button')?.focus();
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      close(true);
    };
    const onDocumentClick = (event: MouseEvent) => {
      if (popoverRef.current?.contains(event.target as Node)) return;
      close(false);
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('click', onDocumentClick);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('click', onDocumentClick);
    };
  }, []);

  const parseNonNegativeNumber = (value: string) => {
    if (value.trim() === '') return null;
    const parsed = Number(value);
    return Number.isFinite(parsed) ? Math.max(0, parsed) : null;
  };
  const outsideScopeCount = differenceCount - inScopeCount;

  return (
    <div
      className="advanced-filters-popover"
      ref={popoverRef}
      role="dialog"
      aria-label="Advanced Filters"
      style={position}
      onClick={(event) => event.stopPropagation()}
    >
      <div className="advanced-filters-head">Advanced Filters<span className="hint">No Rescan</span></div>
      <label className="advanced-filters-row wide">
        <span>Name Masks (FFS Syntax, One per Line)</span>
        <textarea
          className="mono"
          spellCheck={false}
          placeholder={'*/*.log\n/Course/0 Mizzou Courses/.git/'}
          value={maskDraft}
          onChange={(event) => onMaskDraftChange(event.target.value)}
        />
      </label>
      <label className="advanced-filters-row">
        <span>Size (MiB)</span>
        <span className="advanced-filters-pair">
          <input
            type="number" step="any" min="0" placeholder="≥"
            aria-label="Minimum Size in Mebibytes"
            value={advancedFilter.minimumMiB ?? ''}
            onChange={(event) => onAdvancedFilterChange({
              ...advancedFilter,
              minimumMiB: parseNonNegativeNumber(event.target.value),
            })}
          />
          <input
            type="number" step="any" min="0" placeholder="≤"
            aria-label="Maximum Size in Mebibytes"
            value={advancedFilter.maximumMiB ?? ''}
            onChange={(event) => onAdvancedFilterChange({
              ...advancedFilter,
              maximumMiB: parseNonNegativeNumber(event.target.value),
            })}
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
              className={'chip' + (advancedFilter.modifiedWithinDays === days ? ' on' : '')}
              aria-pressed={advancedFilter.modifiedWithinDays === days}
              onClick={() => onAdvancedFilterChange({ ...advancedFilter, modifiedWithinDays: days })}
            >{label}</button>
          ))}
        </div>
      </div>
      <div className="advanced-filters-stat dim" role="status" aria-live="polite">
        {outsideScopeCount > 0
          ? `${outsideScopeCount} differences outside the run scope — they will not run (${inScopeCount} / ${differenceCount} in scope)`
          : `All ${differenceCount} differences are in the run scope`}
      </div>
      <div className="advanced-filters-buttons">
        <button type="button" className="btn" onClick={onClear}>Clear</button>
        <button
          type="button"
          className="btn"
          title="Write the masks above into the job's exclude — from the next Compare on they prune during the scan"
          onClick={() => onWriteMasksToJob(parseScopeMasks(maskDraft))}
        >Write to Job Exclude</button>
        <button
          type="button"
          className="btn accent"
          onClick={() => {
            close(true);
          }}
        >Done</button>
      </div>
    </div>
  );
}
