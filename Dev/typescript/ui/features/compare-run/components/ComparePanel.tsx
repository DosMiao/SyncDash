import { useId, useRef } from 'react';
import { Check, RefreshCw, Square, TriangleAlert, X } from 'lucide-react';
import { humanByteRate, humanSize } from '#core/shared/format.ts';
import { useCssVariables } from '#ui/shared/hooks/useCssVariables.ts';
import { PHASE_LABELS } from '#core/application/progress/runstate.ts';
import type { RunPhase } from '#core/application/progress/runstate.ts';
import type { CompareStage } from '#core/domain/compare/compareProgress.ts';

interface ComparePanelProps {
  stages: CompareStage[];
  cancelling: boolean;
  onCancel: () => void;
}

function CompareProgressFill({ percent }: { percent: number }) {
  const fillRef = useRef<HTMLElement>(null);
  useCssVariables(fillRef, { '--compare-progress-width': `${percent}%` }, [percent]);
  return <i ref={fillRef} />;
}

export function ComparePanel({ stages, cancelling, onCancel }: ComparePanelProps) {
  const titleId = useId();
  return (
    <section className="cmp-panel" aria-labelledby={titleId} aria-busy={stages.some((stage) => !stage.done)}>
      <h2 className="cmp-title" id={titleId}>Comparing</h2>
      <div className="cmp-rows">
        {stages.map((stage) => {
          const rawPercent = stage.bytesTotal > 0 ? (stage.bytesDone / stage.bytesTotal) * 100
            : stage.itemsTotal > 0 ? (stage.itemsDone / stage.itemsTotal) * 100
            : 0;
          // 100 is a phase boundary, not a rounded counter value. The engine can finish reading
          // bytes before it seals/verifies/commits the last item, and PhaseEnd is explicit.
          const percent = stage.done ? 100
            : Math.min(99, Math.max(0, rawPercent));
          const showPercent = stage.done || stage.bytesTotal > 0 || stage.itemsTotal > 0;
          const phaseLabel = PHASE_LABELS[stage.phase as RunPhase] ?? stage.phase;
          return (
            <div
              key={stage.phase}
              className={'stagerow cmp2' + (stage.active ? ' active' : '') + (stage.done ? ' done' : '')}
              aria-current={stage.active ? 'step' : undefined}
            >
              <span className="st-ico" aria-hidden="true">
                {stage.failed ? <TriangleAlert size={13} className="icon-err" />
                  : stage.cancelled ? <Square size={12} />
                    : stage.done ? <Check size={13} />
                      : <RefreshCw size={13} className={stage.active ? 'spin' : ''} />}
              </span>
              <span className="st-name">{phaseLabel}</span>
              <span
                className="st-bar"
                role="progressbar"
                aria-label={`${phaseLabel} progress`}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.floor(percent)}
              ><CompareProgressFill percent={percent} /></span>
              <span className="st-pct">{showPercent ? `${Math.floor(percent)}%` : ''}</span>
              <span className="st-items">
                {stage.label || (stage.itemsTotal ? `${stage.itemsDone} / ${stage.itemsTotal} items` : `${stage.itemsDone} items`)}
              </span>
              <span className="st-bytes">
                {stage.bytesTotal
                  ? `${humanSize(stage.bytesDone) || '0 B'} / ${humanSize(stage.bytesTotal)}`
                  : stage.bytesDone ? humanSize(stage.bytesDone) : ''}
              </span>
              {/* A stage row is one line high and repaints twice a second; below half a MiB/s the
                  reading is noise, so this row shows nothing rather than a flickering rate. */}
              <span className="st-rate">{stage.rate > 512 * 1024 ? humanByteRate(stage.rate) : ''}</span>
              <span className="sr-only" role="status" aria-live="polite">
                {stage.failed ? 'Failed' : stage.cancelled ? 'Cancelled' : stage.done ? 'Complete' : stage.active ? 'In progress' : 'Waiting'}
              </span>
            </div>
          );
        })}
      </div>
      <button type="button" className="btn" disabled={cancelling} onClick={onCancel}>
        {cancelling ? 'Cancelling… (waiting for in-flight chunks)' : <><X size={12} /> Cancel</>}
      </button>
    </section>
  );
}
