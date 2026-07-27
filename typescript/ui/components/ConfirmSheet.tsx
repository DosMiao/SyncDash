import { humanSize } from '../../core/format';
import type { JobDto } from '../../core/types/generated/JobDto';
import type { PreflightDto } from '../../core/types/generated/PreflightDto';

export interface ConfirmTotals {
  copy: number;
  update: number;
  move: number;
  del: number;
  bytes: number;
  delBytes: number;
  flips: number;
  /// Checked rows a filter is hiding — they will NOT run, and that has to be said out loud
  hiddenChecked: number;
}

interface Props {
  job: JobDto;
  totals: ConfirmTotals;
  preflight: PreflightDto | null;
  preflightError: string | null;
  acknowledged: boolean;
  onAcknowledge: (v: boolean) => void;
  onCancel: () => void;
  onConfirm: () => void;
}

export function ConfirmSheet(props: Props) {
  const { job, totals: t, preflight, preflightError, acknowledged, onAcknowledge, onCancel, onConfirm } = props;
  // Gate checks (free disk space / delete ratio) belong in front of the user before Synchronize is
  // pressed, not surfacing at run time in an invisible stderr line
  const blocked = !!preflight && !preflight.ok;

  return (
    <div id="modal" onClick={(e) => { if (e.target === e.currentTarget) onCancel(); }}>
      <div className="sheet">
        <h3>Confirm sync</h3>
        <div id="modal-body">
          <div className="mrow">
            <span>Job</span><b>{job.name}</b><span className={'mode ' + job.mode}>{job.mode}</span>
          </div>
          <div className="mrow">
            <span>Copy / update</span><b>{t.copy} / {t.update}</b>
            <span className="dim">{humanSize(t.bytes) || '0 B'}</span>
          </div>
          <div className="mrow"><span>Move (no re-transfer)</span><b>{t.move}</b></div>
          <div className={'mrow' + (t.del ? ' danger' : '')}>
            <span>Delete (into the trash)</span><b>{t.del}</b>
            <span className="dim">{t.del ? humanSize(t.delBytes) : ''}</span>
          </div>
          {t.flips > 0 && (
            <div className="mrow warn"><span>Of those, reversed</span><b>{t.flips}</b></div>
          )}
          {t.hiddenChecked > 0 && (
            <div className="mrow warn">
              <span>Hidden by filter, not run</span><b>{t.hiddenChecked}</b>
              <span className="dim">The view is the action set</span>
            </div>
          )}
          {preflight?.warnings.map((w, k) => (
            <div className="mrow warn" key={`w${k}`}><span>Warning</span><span className="dim">{w}</span></div>
          ))}
          {blocked && preflight!.blockers.map((b, k) => (
            <div className="mrow danger" key={`b${k}`}><span>Blocked</span><span className="dim">{b}</span></div>
          ))}
          {blocked && (
            <div className="mrow">
              <label>
                <input type="checkbox" checked={acknowledged} onChange={(e) => onAcknowledge(e.target.checked)} />
                {' '}I confirm this is correct, continue (same as the CLI&apos;s --i-know)
              </label>
            </div>
          )}
          {preflightError && (
            <div className="mrow danger"><span>Check failed</span><span className="dim">{preflightError}</span></div>
          )}
        </div>
        <div className="btnrow">
          <button className="btn" onClick={onCancel}>Cancel (Esc)</button>
          <button className="btn accent" disabled={blocked && !acknowledged} onClick={onConfirm}>Apply (Enter)</button>
        </div>
      </div>
    </div>
  );
}
