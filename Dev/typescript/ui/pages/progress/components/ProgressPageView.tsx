import type { MutableRefObject, RefObject } from 'react';
import { Check, Pause, Play, RefreshCw, Square, TriangleAlert } from 'lucide-react';
import { humanDuration, humanSize } from '#core/shared/format.ts';
import {
  PHASE_LABELS,
  activeElapsedMs,
  type RunState,
} from '#core/application/progress/runstate.ts';
import type {
  PowerAction,
  PowerActionCountdown,
  WhenFinishedAction,
} from '#core/application/progress/postRunActions.ts';
import type {
  PausePending,
  PowerActionFailure,
  ProgressPresentation,
  StopState,
} from '../model/progressPresentation.ts';
import { Graph } from './Graph.tsx';

interface ProgressPageViewProps {
  runState: RunState;
  runStateRef: MutableRefObject<RunState>;
  presentation: ProgressPresentation;
  runRejectionMessage: string | null;
  formatByteRate: (runState: RunState) => string;
  formatItemRate: (runState: RunState) => string;
  errorDetailsOpen: boolean;
  onToggleErrorDetails: () => void;
  powerActionCountdown: PowerActionCountdown | null;
  countdownTitleId: string;
  countdownDescriptionId: string;
  countdownProgressFillRef: RefObject<HTMLDivElement | null>;
  countdownCancelButtonRef: RefObject<HTMLButtonElement | null>;
  onCancelPowerActionCountdown: () => void;
  powerActionFailure: PowerActionFailure | null;
  powerActionPending: PowerAction | null;
  onRetryPowerAction: (failure: PowerActionFailure) => void;
  onDismissPowerActionFailure: () => void;
  pausePending: PausePending;
  stopState: StopState;
  onTogglePause: () => void;
  onStop: () => void;
  autoCloseEnabled: boolean;
  onAutoCloseEnabledChange: (enabled: boolean) => void;
  whenFinishedAction: WhenFinishedAction;
  onWhenFinishedActionChange: (action: string) => void;
}

export function ProgressPageView(props: ProgressPageViewProps) {
  const {
    runState,
    runStateRef,
    presentation,
    runRejectionMessage,
    formatByteRate,
    formatItemRate,
    errorDetailsOpen,
    onToggleErrorDetails,
    powerActionCountdown,
    countdownTitleId,
    countdownDescriptionId,
    countdownProgressFillRef,
    countdownCancelButtonRef,
    onCancelPowerActionCountdown,
    powerActionFailure,
    powerActionPending,
    onRetryPowerAction,
    onDismissPowerActionFailure,
    pausePending,
    stopState,
    onTogglePause,
    onStop,
    autoCloseEnabled,
    onAutoCloseEnabledChange,
    whenFinishedAction,
    onWhenFinishedActionChange,
  } = props;
  const {
    roundedCompletionPercentage,
    runPaused,
    finalizing,
    estimatedTimeRemaining,
    progressValueText,
    summaryStatusText,
    errorCount,
    warningCount,
  } = presentation;

  return (
    <div className={'pwin' + (runState.applying ? ' applying' : '')}>
      <div className="phead">
        <span className="pjob">SyncDash</span>
        <span className="pphase" role="status" aria-live="polite" aria-atomic="true">
          {summaryStatusText !== null
            ? summaryStatusText
            : runRejectionMessage ? `Could not start — ${runRejectionMessage}`
            : runState.running
              ? <>
                  {runPaused && <><Pause size={12} /> Paused — </>}
                  {runState.phase ? PHASE_LABELS[runState.phase] : ''}
                  {finalizing ? ' — Finalizing' : ''}
                </>
              : 'Waiting for a run…'}
        </span>
        <span
          className={'ppct ' + (runState.summary
            ? (runState.summary.cancelled || runState.summary.errors ? 'err' : 'ok')
            : runPaused ? 'paused' : '')}
          role="progressbar"
          aria-label="Apply progress"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={runState.applying || runState.summary
            ? roundedCompletionPercentage
            : undefined}
          aria-valuetext={progressValueText}
        >
          {runState.summary
            ? (runState.summary.cancelled ? 'Stopped' : `${roundedCompletionPercentage}%`)
            : runRejectionMessage ? 'Failed'
            : runState.running
              ? (runState.applying ? `${roundedCompletionPercentage}%` : '…')
              : '—'}
        </span>
      </div>

      <div className="stagerows">
        {runState.stages.map((stage) => (
          <div
            key={stage.phase}
            className={'stagerow' + (stage.active ? ' active' : '') + (stage.done ? ' done' : '')}
          >
            <span className="st-ico">
              {stage.active ? <RefreshCw size={13} className="spin" />
                : stage.failed ? <TriangleAlert size={13} className="icon-err" />
                  : stage.cancelled ? <Square size={12} />
                    : stage.done ? <Check size={13} />
                      : <RefreshCw size={13} />}
            </span>
            <span className="st-name">{PHASE_LABELS[stage.phase]}</span>
            <span className="st-detail">{stage.detail}</span>
          </div>
        ))}
      </div>

      <div className="graphs">
        <Graph
          caption="Data (cumulative bytes)"
          metric="bytesDone"
          runRef={runStateRef}
          rateText={formatByteRate}
        />
        <Graph
          caption="Items (cumulative count)"
          metric="itemsDone"
          runRef={runStateRef}
          rateText={formatItemRate}
        />
        <div className="readouts">
          <div className="rh" /><div className="rh">Processed</div><div className="rh">Remaining</div>
          <div className="rh">Items</div>
          <div>{runState.completed.items} / {runState.totals.items}</div>
          <div>{Math.max(0, runState.totals.items - runState.completed.items)}</div>
          <div className="rh">Bytes</div>
          <div>{humanSize(runState.completed.bytes)} / {humanSize(runState.totals.bytes)}</div>
          <div>{humanSize(Math.max(0, runState.totals.bytes - runState.completed.bytes))}</div>
          <div className="rh">Time</div>
          <div>{humanDuration(runState.summary
            ? runState.summary.elapsed_ms ?? 0
            : activeElapsedMs(runState))}</div>
          <div>{estimatedTimeRemaining}</div>
        </div>
        <div className="curfile" title={runState.currentPath}>
          {runState.currentPath ? `‎${runState.currentPath}` : ''}
        </div>
      </div>

      <div className={'errsec'
        + (runState.errors.length ? ' show' : '')
        + (errorDetailsOpen ? ' open' : '')}>
        <button
          type="button"
          className="errhead"
          aria-expanded={errorDetailsOpen}
          aria-controls="progress-errors"
          onClick={onToggleErrorDetails}
        >
          <TriangleAlert size={14} className={errorCount ? 'icon-err' : 'icon-warn'} />
          <span className="cnt-err">
            {errorCount ? `${errorCount} ${errorCount === 1 ? 'error' : 'errors'}` : ''}
          </span>
          <span className="cnt-warn">
            {warningCount ? `${warningCount} ${warningCount === 1 ? 'warning' : 'warnings'}` : ''}
          </span>
          <span className="dim errtip">{errorDetailsOpen ? 'Collapse details' : 'Expand details'}</span>
        </button>
        <div id="progress-errors" className="errlist">
          {runState.errors.map((entry, index) => (
            <div key={index} className={'erow' + (entry.warning ? ' warn' : '')}>
              <span className="epath mono">{entry.path}</span>{' '}
              <span className="emsg">
                {entry.side ? `[${entry.action}/${entry.side}] ` : `[${entry.action}] `}{entry.message}
              </span>
            </div>
          ))}
        </div>
      </div>

      {powerActionCountdown && (
        <div
          className="countdown show"
          role="alertdialog"
          aria-labelledby={countdownTitleId}
          aria-describedby={countdownDescriptionId}
        >
          <span id={countdownTitleId} className="cdtext">
            <span aria-hidden="true">
              {powerActionCountdown.action === 'sleep' ? 'Sleep' : 'Shut down'} in{' '}
              {powerActionCountdown.secondsRemaining}s
            </span>
            <span className="sr-only">
              {powerActionCountdown.action === 'sleep' ? 'Computer sleep scheduled' : 'Computer shutdown scheduled'}
            </span>
          </span>
          <span id={countdownDescriptionId} className="sr-only">
            Press Escape or activate Cancel to keep the computer running.
          </span>
          <span className="sr-only" role="alert" aria-live="assertive" aria-atomic="true">
            {powerActionCountdown.action === 'sleep'
              ? 'SyncDash will put the computer to sleep in 10 seconds.'
              : 'SyncDash will shut down the computer in 10 seconds.'}
          </span>
          <div className="cdbar" aria-hidden="true">
            <div ref={countdownProgressFillRef} className="cdfill" />
          </div>
          <button
            ref={countdownCancelButtonRef}
            type="button"
            className="btn"
            autoFocus
            onClick={onCancelPowerActionCountdown}
          >Cancel</button>
        </div>
      )}

      {powerActionFailure && (
        <div className="countdown show" role="alert">
          <span className="cdtext">{powerActionFailure.error}</span>
          <button
            type="button"
            className="btn"
            disabled={powerActionPending !== null
              || powerActionFailure.runId !== runState.runId
              || runState.running
              || !runState.summary}
            onClick={() => onRetryPowerAction(powerActionFailure)}
          >{powerActionPending ? 'Retrying…' : 'Retry'}</button>
          <button type="button" className="btn" onClick={onDismissPowerActionFailure}>
            Dismiss
          </button>
        </div>
      )}

      <div className="controls">
        <button
          type="button"
          className="btn"
          disabled={!runState.running
            || !!runState.summary
            || pausePending !== null
            || stopState !== 'idle'}
          onClick={onTogglePause}
        >{pausePending === 'pause'
            ? 'Pausing…'
            : pausePending === 'resume'
              ? 'Resuming…'
              : runPaused
                ? <><Play size={12} /> Continue</>
                : <><Pause size={12} /> Pause</>}</button>
        <button
          type="button"
          className="btn btn-stop"
          disabled={!runState.running || !!runState.summary || stopState !== 'idle'}
          onClick={onStop}
        >
          {stopState === 'idle'
            ? <><Square size={12} /> Stop</>
            : stopState === 'stopping' ? 'Stopping…' : 'Finished'}
        </button>
        <label className="dim chkline">
          <input
            type="checkbox"
            checked={autoCloseEnabled}
            disabled={powerActionPending !== null}
            onChange={(event) => onAutoCloseEnabledChange(event.target.checked)}
          /> Auto-close when finished
        </label>
        <label className="wfin">
          When finished
          <select
            value={whenFinishedAction}
            disabled={autoCloseEnabled || powerActionPending !== null}
            title={autoCloseEnabled
              ? 'Auto-close takes precedence over a post-run power action'
              : undefined}
            onChange={(event) => onWhenFinishedActionChange(event.target.value)}
          >
            <option value="none">Do nothing</option>
            <option value="sleep">Sleep</option>
            <option value="shutdown">Shut down</option>
          </select>
        </label>
      </div>
    </div>
  );
}
