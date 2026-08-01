import { Check, RefreshCw, Square, TriangleAlert, X } from 'lucide-react';
import { humanSize } from '../../core/format';
import type { CmpStage } from '../../core/compareProgress';

export const CMP_LABEL: Record<string, string> = {
  'scan-source': 'Scan source', 'scan-target': 'Scan target', 'compare': 'Compare',
  'refresh': 'Refresh archive', 'archive': 'Save archive',
};

interface Props {
  stages: CmpStage[];
  cancelling: boolean;
  onCancel: () => void;
}

/// v0.9.1: compare progress renders in place in the diff-table area — the main window is already in
/// front, so no separate window (no flash on small trees, live per-side counts on big ones, no
/// child-window lifecycle).
export function ComparePanel({ stages, cancelling, onCancel }: Props) {
  return (
    <div className="cmp-panel">
      <div className="cmp-title">Comparing</div>
      <div className="cmp-rows">
        {stages.map((s) => {
          const rawPct = s.bytesTotal > 0 ? (s.bytesDone / s.bytesTotal) * 100
            : s.itemsTotal > 0 ? (s.itemsDone / s.itemsTotal) * 100
            : 0;
          // 100 is a phase boundary, not a rounded counter value. The engine can finish reading
          // bytes before it seals/verifies/commits the last item, and PhaseEnd is explicit.
          const pct = s.done ? 100
            : Math.min(99, Math.max(0, rawPct));
          const showPct = s.done || s.bytesTotal > 0 || s.itemsTotal > 0;
          return (
            <div key={s.phase} className={'stagerow cmp2' + (s.active ? ' active' : '') + (s.done ? ' done' : '')}>
              <span className="st-ico">
                {s.failed ? <TriangleAlert size={13} className="icon-err" />
                  : s.cancelled ? <Square size={12} />
                    : s.done ? <Check size={13} />
                      : <RefreshCw size={13} className={s.active ? 'spin' : ''} />}
              </span>
              <span className="st-name">{CMP_LABEL[s.phase] ?? s.phase}</span>
              <span className="st-bar"><i style={{ width: `${pct}%` }} /></span>
              <span className="st-pct">{showPct ? `${Math.floor(pct)}%` : ''}</span>
              <span className="st-items">
                {s.label || (s.itemsTotal ? `${s.itemsDone} / ${s.itemsTotal} items` : `${s.itemsDone} items`)}
              </span>
              <span className="st-bytes">
                {s.bytesTotal
                  ? `${humanSize(s.bytesDone) || '0 B'} / ${humanSize(s.bytesTotal)}`
                  : s.bytesDone ? humanSize(s.bytesDone) : ''}
              </span>
              <span className="st-rate">{s.rate > 512 * 1024 ? `${(s.rate / (1 << 20)).toFixed(1)} MiB/s` : ''}</span>
            </div>
          );
        })}
      </div>
      <button className="btn" disabled={cancelling} onClick={onCancel}>
        {cancelling ? 'Cancelling… (waiting for in-flight chunks)' : <><X size={12} /> Cancel</>}
      </button>
    </div>
  );
}
