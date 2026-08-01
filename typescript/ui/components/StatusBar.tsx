import { Minus, Plus } from 'lucide-react';
import { humanSize } from '../../core/format';
import type { PlanDto } from '../../core/plan';
import type { StatusState } from '../hooks/useStatus';
import type { ResultView } from '../state/result-workspace';

interface StatusBarProps {
  status: StatusState;
  onUndo: () => void;
  plan: PlanDto | null;
  inScopeCount: number;
  resultView: ResultView;
  zoom: number;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onZoomReset: () => void;
}

/// Returned as parts rather than one joined string so the exclusion count can be tinted: a filter
/// having swallowed a whole tree is the one entry here that is a warning rather than a statistic,
/// and color says that where a ⚠ glyph could not (it renders as a color emoji and ignores `color`).
function counts(
  plan: PlanDto,
  inScopeCount: number,
  resultView: ResultView,
): { parts: { text: string; warn?: boolean }[]; title: string } {
  const differenceCount = plan.ops.length;
  const header = plan.header;
  const identicalCount = plan.identical_count ?? 0;
  const parts: { text: string; warn?: boolean }[] = resultView === 'identical'
    ? [{ text: `Identical ${identicalCount.toLocaleString()}` }]
    : [{ text: `In Scope ${inScopeCount.toLocaleString()} / ${differenceCount.toLocaleString()}` }];
  if (resultView === 'differences' && inScopeCount < differenceCount) {
    parts.push({ text: `${(differenceCount - inScopeCount).toLocaleString()} Outside Scope, Not Run` });
  }
  parts.push({ text: `Scanned ${header.source_entries.toLocaleString()} ⇄ ${header.target_entries.toLocaleString()}` });
  if (resultView === 'differences') parts.push({ text: `Identical ${identicalCount.toLocaleString()}` });
  // Excludes are always spelled out so an empty plan cannot become a whole-root identity claim.
  const sourceExcluded = header.source_excluded ?? 0;
  const targetExcluded = header.target_excluded ?? 0;
  if (sourceExcluded + targetExcluded > 0) {
    parts.push({ text: `Excluded ${sourceExcluded.toLocaleString()} ⇄ ${targetExcluded.toLocaleString()}`, warn: true });
  }
  return {
    parts,
    title: `${differenceCount.toLocaleString()} differences planned; ${identicalCount.toLocaleString()} files identical on both sides (${humanSize(plan.identical_bytes ?? 0)})`
      + (sourceExcluded + targetExcluded > 0
        ? `\nFilters (including the built-in default excludes such as .git/node_modules) removed ${sourceExcluded} source / ${targetExcluded} target entries — they take no part in the compare or the run. To synchronize them too: edit the job and turn the built-in default excludes off.`
        : ''),
  };
}

export function StatusBar(props: StatusBarProps) {
  const { status, onUndo, plan, inScopeCount, resultView, zoom, onZoomIn, onZoomOut, onZoomReset } = props;
  const resultCounts = plan ? counts(plan, inScopeCount, resultView) : null;

  return (
    <footer className={'status ' + status.cls} aria-label="Application status">
      <span
        className="statusmsg"
        title={status.msg}
        role={status.cls === 'err' ? 'alert' : 'status'}
        aria-live={status.cls === 'err' ? 'assertive' : 'polite'}
        aria-atomic="true"
      >{status.msg}</span>
      {status.undo && <button type="button" className="linkbtn" onClick={onUndo}>{status.undo.label}</button>}
      {/* Always rendered, even when empty: .counts carries the margin-left:auto that pushes the right
          group over, and dropping the element would let the zoom stepper slide back to the left. */}
      <span className="counts" title={resultCounts?.title} aria-label={resultCounts?.title}>
        {resultCounts?.parts.map((part, index) => (
          <span key={part.text} className={part.warn ? 'count-warn' : undefined}>
            {index > 0 ? ' · ' : ''}{part.text}
          </span>
        ))}
      </span>
      <span className="zoombox" role="group" aria-label="Zoom controls">
        <button type="button" className="zbtn" title="Zoom out (Ctrl+−)" aria-label="Zoom out" onClick={onZoomOut}><Minus size={12} /></button>
        <button
          type="button"
          className="zoomval"
          title="Reset zoom (Ctrl+0)"
          aria-label={`Reset zoom, currently ${Math.round(zoom * 100)}%`}
          onClick={onZoomReset}
        >{Math.round(zoom * 100)}%</button>
        <button type="button" className="zbtn" title="Zoom in (Ctrl++)" aria-label="Zoom in" onClick={onZoomIn}><Plus size={12} /></button>
      </span>
    </footer>
  );
}
