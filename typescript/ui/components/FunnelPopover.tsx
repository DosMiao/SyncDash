import { useEffect, useLayoutEffect, useRef, useState } from 'react';
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
  const [pos, setPos] = useState({ left: anchor.left, top: anchor.bottom + 4 });
  const [maskText, setMaskText] = useState(vfilter.masks.join('\n'));

  useLayoutEffect(() => {
    const w = ref.current?.offsetWidth ?? 400;
    setPos({ left: Math.max(6, Math.min(anchor.left, window.innerWidth - w - 6)), top: anchor.bottom + 4 });
  }, [anchor]);

  // Mask matching costs an IPC round trip per keystroke otherwise
  useEffect(() => {
    const t = setTimeout(() => {
      const masks = maskText.split('\n').map((s) => s.trim()).filter(Boolean);
      if (masks.join('\n') !== vfilter.masks.join('\n')) onChange({ ...vfilter, masks });
    }, 200);
    return () => clearTimeout(t);
  }, [maskText]);

  const num = (s: string) => (s.trim() === '' ? null : Math.max(0, Number(s)));
  const hidden = planned - shown;

  return (
    <div id="funnelpop" ref={ref} style={pos} onClick={(e) => e.stopPropagation()}>
      <div className="fp-head">Filter current results<span className="dim thin">no rescan</span></div>
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
            value={vfilter.minMB ?? ''}
            onChange={(e) => onChange({ ...vfilter, minMB: num(e.target.value) })}
          />
          <input
            type="number" step="any" min="0" placeholder="≤"
            value={vfilter.maxMB ?? ''}
            onChange={(e) => onChange({ ...vfilter, maxMB: num(e.target.value) })}
          />
        </span>
      </label>
      <div className="fp-row">
        <span>Modified</span>
        <div className="fp-chips">
          {DAYS.map(([label, d]) => (
            <button
              key={label}
              className={'chip' + (vfilter.days === d ? ' on' : '')}
              onClick={() => onChange({ ...vfilter, days: d })}
            >{label}</button>
          ))}
        </div>
      </div>
      <div className="fp-stat dim">
        {hidden > 0
          ? `${hidden} items hidden — those rows will not run (showing ${shown} / ${planned})`
          : `No rows hidden (${planned} items in total)`}
      </div>
      <div className="fp-btns">
        <button className="btn" onClick={() => { setMaskText(''); onClear(); }}>Clear</button>
        <button
          className="btn"
          title="Write the masks above into the job's exclude — from the next Compare on they prune during the scan"
          onClick={() => onPromote(maskText.split('\n').map((s) => s.trim()).filter(Boolean))}
        >Write to job exclude</button>
        <button className="btn accent" onClick={onDone}>Done</button>
      </div>
    </div>
  );
}
