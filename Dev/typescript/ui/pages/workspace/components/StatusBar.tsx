import { Minus, Plus, X } from 'lucide-react';
import { formatCount, humanSize } from '#core/shared/format.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';
import type { StatusState } from '#ui/shared/status/useStatus.ts';
import type { CompareResultView } from '#core/application/compare-workspace/compareWorkspaceModel.ts';

interface StatusBarProps {
  status: StatusState;
  onAction: (actionId?: number) => void;
  onDismissNotice: (noticeId: number) => void;
  plan: PlanDto | null;
  inScopeCount: number;
  resultView: CompareResultView;
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
  resultView: CompareResultView,
): { parts: { text: string; warn?: boolean }[]; title: string } {
  const differenceCount = plan.ops.length;
  const header = plan.header;
  const identicalCount = plan.identical_count ?? 0;
  const parts: { text: string; warn?: boolean }[] = resultView === 'identical'
    ? [{ text: `Identical ${formatCount(identicalCount)}` }]
    : [{ text: `In Scope ${formatCount(inScopeCount)} / ${formatCount(differenceCount)}` }];
  if (resultView === 'differences' && inScopeCount < differenceCount) {
    parts.push({ text: `${formatCount(differenceCount - inScopeCount)} Outside Scope, Not Run` });
  }
  parts.push({ text: `Scanned ${formatCount(header.source_entries)} ⇄ ${formatCount(header.target_entries)}` });
  if (resultView === 'differences') parts.push({ text: `Identical ${formatCount(identicalCount)}` });
  // Excludes are always spelled out so an empty plan cannot become a whole-root identity claim.
  const sourceExcluded = header.source_excluded ?? 0;
  const targetExcluded = header.target_excluded ?? 0;
  if (sourceExcluded + targetExcluded > 0) {
    parts.push({ text: `Excluded ${formatCount(sourceExcluded)} ⇄ ${formatCount(targetExcluded)}`, warn: true });
  }
  return {
    parts,
    title: `${formatCount(differenceCount)} differences planned; ${formatCount(identicalCount)} files identical on both sides (${humanSize(plan.identical_bytes ?? 0)})`
      + (sourceExcluded + targetExcluded > 0
        ? `\nSaved filters (including any junk-preset patterns) removed ${formatCount(sourceExcluded)} source / ${formatCount(targetExcluded)} target entries — they take no part in the Compare result or Synchronize. To include them, edit the job's Filters section and remove the matching exclude patterns.`
        : ''),
  };
}

export function StatusBar(props: StatusBarProps) {
  const {
    status,
    onAction,
    onDismissNotice,
    plan,
    inScopeCount,
    resultView,
    zoom,
    onZoomIn,
    onZoomOut,
    onZoomReset,
  } = props;
  const resultCounts = plan ? counts(plan, inScopeCount, resultView) : null;

  return (
    <footer className={'status ' + status.appearance} aria-label="Application status">
      {status.notices.length > 0 && (
        <div className="status-notices" aria-label="Background action failures">
          {status.notices.map((notice) => {
            const noticeAction = notice.action;
            return (
              <div key={notice.id} className={`status-notice ${notice.appearance}`} role="alert">
                <span>{notice.message}</span>
                {noticeAction && (
                  <button
                    type="button"
                    className="linkbtn"
                    disabled={status.actionPending}
                    onClick={() => onAction(noticeAction.id)}
                  >Retry {noticeAction.label}</button>
                )}
                <button
                  type="button"
                  className="icon-btn"
                  aria-label="Dismiss background action failure"
                  disabled={status.actionPending && noticeAction === null}
                  onClick={() => onDismissNotice(notice.id)}
                ><X size={12} /></button>
              </div>
            );
          })}
        </div>
      )}
      <div className="status-primary">
        <span
          className="statusmsg"
          title={status.message}
          role={status.appearance === 'err' ? 'alert' : 'status'}
          aria-live={status.appearance === 'err' ? 'assertive' : 'polite'}
          aria-atomic="true"
        >{status.message}</span>
        {status.action && (
          <button
            type="button"
            className="linkbtn"
            disabled={status.actionPending}
            onClick={() => onAction()}
          >{status.action.label}</button>
        )}
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
      </div>
    </footer>
  );
}
