import { humanSize } from '../../core/format';
import type { PlanDto } from '../../core/plan';
import type { StatusState } from '../hooks/useStatus';

interface Props {
  status: StatusState;
  onUndo: () => void;
  plan: PlanDto | null;
  visibleCount: number;
  zoom: number;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onZoomReset: () => void;
}

/// The FFS status-bar line "Showing 481 of 23,112 items": showing / planned / scanned on both sides /
/// judged identical. Without it, when a file is missing from the table you can't tell whether it was
/// "identical" or simply "never scanned".
function counts(plan: PlanDto, vis: number): { text: string; title: string } {
  const tot = plan.ops.length;
  const h = plan.header;
  const parts = [`Showing ${vis} / ${tot}`];
  if (vis < tot) parts.push(`${tot - vis} hidden, not run`);
  parts.push(`Scanned ${h.source_entries.toLocaleString()} ⇄ ${h.target_entries.toLocaleString()}`);
  parts.push(`Identical ${(plan.equal_count ?? 0).toLocaleString()}`);
  // Excludes are always spelled out — never let "both sides identical ✓" hide a tree the filter swallowed
  const exS = h.source_excluded ?? 0, exT = h.target_excluded ?? 0;
  if (exS + exT > 0) parts.push(`⚠ Excluded ${exS.toLocaleString()} ⇄ ${exT.toLocaleString()}`);
  return {
    text: parts.join(' · '),
    title: `${tot} items planned; ${(plan.equal_count ?? 0).toLocaleString()} files identical on both sides (${humanSize(plan.equal_bytes ?? 0)})`
      + (exS + exT > 0
        ? `\n⚠ Filters (including the built-in default excludes such as .git/node_modules) removed ${exS} source / ${exT} target entries — they take no part in the compare or the run. To synchronize them too: edit the job and turn the built-in default excludes off.`
        : ''),
  };
}

export function StatusBar(props: Props) {
  const { status, onUndo, plan, visibleCount, zoom, onZoomIn, onZoomOut, onZoomReset } = props;
  const c = plan ? counts(plan, visibleCount) : null;

  return (
    <footer id="status" className={status.cls}>
      <span id="statusmsg" title={status.msg}>{status.msg}</span>
      {status.undo && <button className="linkbtn" onClick={onUndo}>{status.undo.label}</button>}
      {/* Always rendered, even when empty: .counts carries the margin-left:auto that pushes the right
          group over, and dropping the element would let the zoom stepper slide back to the left. */}
      <span className="counts" title={c?.title}>{c?.text ?? ''}</span>
      <span id="zoombox">
        <button className="zbtn" title="Zoom out (Ctrl+−)" onClick={onZoomOut}>−</button>
        <span id="zoomval" title="Reset zoom (Ctrl+0)" onClick={onZoomReset}>{Math.round(zoom * 100)}%</span>
        <button className="zbtn" title="Zoom in (Ctrl++)" onClick={onZoomIn}>+</button>
      </span>
    </footer>
  );
}
