import { Check, RefreshCw, X } from 'lucide-react';
import { humanSize } from '../../core/format';

export const CMP_LABEL: Record<string, string> = {
  'scan-source': 'Scan source', 'scan-target': 'Scan target', 'compare': 'Compare', 'refresh': 'Refresh archive',
};

export interface CmpStage {
  phase: string;
  label: string;
  itemsDone: number;
  itemsTotal: number;
  bytesDone: number;
  bytesTotal: number;
  /// Smoothed bytes/second; 0 while it is still too noisy to show
  rate: number;
  active: boolean;
  done: boolean;
}

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
          const pct = s.done ? 100
            : s.bytesTotal > 0 ? (s.bytesDone / s.bytesTotal) * 100
            : s.itemsTotal > 0 ? (s.itemsDone / s.itemsTotal) * 100
            : 0;
          const showPct = s.done || s.bytesTotal > 0 || s.itemsTotal > 0;
          return (
            <div key={s.phase} className={'stagerow cmp2' + (s.active ? ' active' : '') + (s.done ? ' done' : '')}>
              <span className="st-ico">
                {s.done ? <Check size={13} /> : <RefreshCw size={13} className={s.active ? 'spin' : ''} />}
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
