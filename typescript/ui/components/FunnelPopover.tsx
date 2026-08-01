import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { clamp } from './ui';
import type { ViewFilter } from '../../core/filter';

interface Props {
  anchor: DOMRect;
  vfilter: ViewFilter;
  /// Rows currently shown vs. planned, for the "these will not run" line
  shown: number;
  planned: number;
  onChange: (next: ViewFilter) => void;
  onClear: () => void;
  onPromote: (masks: string[]) => void;
  onDone: () => void;
}

const DAYS: [string, number | null][] = [['Any', null], ['Today', 1], ['7 days', 7], ['30 days', 30]];

/// P3 funnel: a view filter over the current compare result (no rescan).
/// Same semantics as FFS: **the view is the action set** — rows hidden here do not run. The confirm sheet
/// spells that out, because it overturns the old intuition that "checked means it runs".
export function FunnelPopover(props: Props) {
  const { anchor, vfilter, shown, planned, onChange, onClear, onPromote, onDone } = props;
  const ref = useRef<HTMLDivElement>(null);
  const previousFocus = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const [pos, setPos] = useState({ left: anchor.left, top: anchor.bottom + 4 });
  const [maskText, setMaskText] = useState(vfilter.masks.join('\n'));
  const latest = useRef({ vfilter, onChange, onDone });
  latest.current = { vfilter, onChange, onDone };

  // Clamped on both axes: this panel is tall, and opened from a filter bar low in a short window it
  // used to run straight off the bottom with the buttons unreachable
  useLayoutEffect(() => {
    const el = ref.current;
    setPos(clamp(anchor.left, anchor.bottom + 4, el?.offsetWidth ?? 400, el?.offsetHeight ?? 380));
  }, [anchor]);

  useLayoutEffect(() => {
    ref.current?.querySelector<HTMLElement>('textarea, input, button')?.focus();
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      e.stopPropagation();
      latest.current.onDone();
      const previous = previousFocus.current;
      if (previous?.isConnected) requestAnimationFrame(() => previous.focus());
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);

  // Mask matching costs an IPC round trip per keystroke otherwise
  useEffect(() => {
    const t = setTimeout(() => {
      const masks = maskText.split('\n').map((s) => s.trim()).filter(Boolean);
      const current = latest.current;
      if (masks.join('\n') !== current.vfilter.masks.join('\n')) {
        current.onChange({ ...current.vfilter, masks });
      }
    }, 200);
    return () => clearTimeout(t);
  }, [maskText]);

  const num = (s: string) => (s.trim() === '' ? null : Math.max(0, Number(s)));
  const hidden = planned - shown;

  return (
    <div
      className="funnelpop"
      ref={ref}
      role="dialog"
      aria-label="Filter current results"
      style={pos}
      onClick={(e) => e.stopPropagation()}
    >
      <div className="fp-head">Filter current results<span className="hint">no rescan</span></div>
      <label className="fp-row wide">
        <span>Name masks (FFS syntax, one per line)</span>
        <textarea
          className="mono"
          spellCheck={false}
          placeholder={'*/*.log\n/Course/0 Mizzou Courses/.git/'}
          value={maskText}
          onChange={(e) => setMaskText(e.target.value)}
        />
      </label>
      <label className="fp-row">
        <span>Size (MB)</span>
        <span className="fp-pair">
          <input
            type="number" step="any" min="0" placeholder="≥"
            aria-label="Minimum size in megabytes"
            value={vfilter.minMB ?? ''}
            onChange={(e) => onChange({ ...vfilter, minMB: num(e.target.value) })}
          />
          <input
            type="number" step="any" min="0" placeholder="≤"
            aria-label="Maximum size in megabytes"
            value={vfilter.maxMB ?? ''}
            onChange={(e) => onChange({ ...vfilter, maxMB: num(e.target.value) })}
          />
        </span>
      </label>
      <div className="fp-row" role="group" aria-label="Modified date">
        <span>Modified</span>
        <div className="fp-chips">
          {DAYS.map(([label, d]) => (
            <button
              type="button"
              key={label}
              className={'chip' + (vfilter.days === d ? ' on' : '')}
              aria-pressed={vfilter.days === d}
              onClick={() => onChange({ ...vfilter, days: d })}
            >{label}</button>
          ))}
        </div>
      </div>
      <div className="fp-stat dim" role="status" aria-live="polite">
        {hidden > 0
          ? `${hidden} items hidden — those rows will not run (showing ${shown} / ${planned})`
          : `No rows hidden (${planned} items in total)`}
      </div>
      <div className="fp-btns">
        <button type="button" className="btn" onClick={() => { setMaskText(''); onClear(); }}>Clear</button>
        <button
          type="button"
          className="btn"
          title="Write the masks above into the job's exclude — from the next Compare on they prune during the scan"
          onClick={() => onPromote(maskText.split('\n').map((s) => s.trim()).filter(Boolean))}
        >Write to job exclude</button>
        <button
          type="button"
          className="btn accent"
          onClick={() => {
            onDone();
            const previous = previousFocus.current;
            if (previous?.isConnected) requestAnimationFrame(() => previous.focus());
          }}
        >Done</button>
      </div>
    </div>
  );
}
