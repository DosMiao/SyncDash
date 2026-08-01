// SyncDash progress sub-window (FFS-equivalent behavior parameters):
// - rate = 4s sliding window, ETA = 60s sliding window, readouts refresh every 500ms, redraw every 100ms
// - percentage = (bytesDone + itemsDone) / (bytesTotal + itemsTotal) (same formula as FFS)
// - paused time is excluded from elapsed; Stop = cooperative cancel (stops after the current chunk, so
//   the end state never leaves a half-written file)
// - errors accumulate without interrupting; Auto-close and When-finished (sleep/shut down, 10s cancellable countdown)
// Data source: the `run-progress` event broadcast by the Rust TauriSink (throttled to ≥100ms apiece, carries run_id).
//
// The run state lives in a ref, not in useState: events arrive up to ten times a second and the sample
// series runs to thousands of entries. Rendering is driven by the same 100 ms tick the original used, so
// the throttle stays exactly where FFS put it instead of being whatever React decides to coalesce.

import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, Pause, Play, RefreshCw, Square, TriangleAlert } from 'lucide-react';
import { emit, listen } from '@tauri-apps/api/event';
import { getCurrentWindow, ProgressBarStatus } from '@tauri-apps/api/window';
import {
  cancelRun,
  closeProgressLaunch,
  destroyProgressWindow,
  pauseRun,
  postSyncAction,
  replayRunEvents,
} from '../core/ipc';
import { humanDuration, humanSize } from '../core/format';
import { mergeRunEventReplay } from '../core/runEvents';
import { applyZoom, readZoom } from '../core/zoom';
import { Graph } from './Graph';
import type { RunEv, RunState, Sample } from './runstate';
import { PHASE_LABEL, activeNow, endStage, newRunState, percent, startStage, windowRate } from './runstate';

const win = getCurrentWindow();

interface PendingLaunch { id: number; afterRunId: number }
interface RunRejected { launch_id: number; message: string }

export function ProgressApp() {
  const run = useRef<RunState>(newRunState());
  const pendingLaunch = useRef<PendingLaunch | null>(null);
  // Bumping this is the only thing that re-renders; the tick below decides how often that happens
  const [, forceRender] = useState(0);
  const rerender = useCallback(() => forceRender((n) => n + 1), []);

  const [autoclose, setAutoclose] = useState(() => localStorage.getItem('sd.autoclose') === '1');
  const [whenFin, setWhenFin] = useState(() => localStorage.getItem('sd.whenfin') ?? 'none');
  const [countdown, setCountdown] = useState<{ kind: string; left: number } | null>(null);
  const [errorsOpen, setErrorsOpen] = useState(false);
  const [rejected, setRejected] = useState<string | null>(null);
  /// Where the Stop button is in its own little lifecycle. A state rather than the button's caption:
  /// the enabled test used to be `label !== '■ Stop'`, which quietly turns into "always disabled" the
  /// day anyone edits the wording.
  const [stop, setStop] = useState<'idle' | 'stopping' | 'finished'>('idle');

  const reportControlError = useCallback((action: string, error: unknown) => {
    run.current.errors.push({
      path: '', action: 'control', side: '', message: `${action}: ${String(error)}`, warning: false,
    });
    setErrorsOpen(true);
    setStop('idle');
    rerender();
  }, [rerender]);

  const autocloseRef = useRef(autoclose);
  autocloseRef.current = autoclose;
  const whenFinRef = useRef(whenFin);
  whenFinRef.current = whenFin;

  const armLaunch = useCallback((id: number) => {
    const afterRunId = run.current.runId;
    pendingLaunch.current = { id, afterRunId };
    const reset = newRunState();
    reset.runId = afterRunId;
    run.current = reset;
    setRejected(null);
    setStop('idle');
    setCountdown(null);
    rerender();
    void emit(`progress-window-ready-${id}`);
  }, [rerender]);

  useEffect(() => {
    // This window opens fresh each run, so it has to re-assert the scale the main window is using
    void applyZoom(readZoom());
  }, []);

  // The instantaneous rate labels use the 4 s window; they are painted inside <Graph> at 10 Hz
  const rateBytes = useCallback((st: RunState) => {
    const r = windowRate(st, 4000);
    return r ? `${(r.bps / (1 << 20)).toFixed(2)} MiB/s` : '';
  }, []);
  const rateItems = useCallback((st: RunState) => {
    const r = windowRate(st, 4000);
    return r ? `${r.ips.toFixed(0)} items/s` : '';
  }, []);

  // Countdown before sleep / shut down — 10 s, cancellable
  useEffect(() => {
    if (!countdown) return;
    if (countdown.left <= 0) {
      setCountdown(null);
      postSyncAction(countdown.kind).catch(() => {});
      return;
    }
    const t = setTimeout(() => setCountdown((c) => (c ? { ...c, left: c.left - 1 } : c)), 1000);
    return () => clearTimeout(t);
  }, [countdown]);

  const onEvent = useCallback((ev: RunEv) => {
    const s = run.current;
    if (ev.purpose === 'compare') return;   // compare belongs to the main-window panel; this window only shows apply
    const pending = pendingLaunch.current;
    if (pending && ev.run_id <= pending.afterRunId) return;
    if (ev.run_id < s.runId) return;        // late event from an already-cancelled run
    if (ev.run_id > s.runId) {
      const closeAfterStop = s.closeAfterStop;
      run.current = newRunState(ev.run_id, ev.ts_ms);
      run.current.closeAfterStop = closeAfterStop;
      setRejected(null);
      setStop('idle');
      setCountdown(null);
    }
    const st = run.current;

    switch (ev.kind) {
      case 'phase_start': {
        const p = ev.phase!;
        st.phase = p;
        let row = st.stages.find((x) => x.phase === p);
        if (!row) { row = { phase: p, detail: '', active: true, done: false }; st.stages.push(row); }
        startStage(row);
        if (ev.label) row.detail = ev.label;
        st.applying = true;
        // The headline meter and graphs describe one coherent active phase. Reusing apply's done
        // counters with refresh/ship totals produced arbitrary percentages and negative rates.
        st.totals = { items: ev.items_total ?? 0, bytes: ev.bytes_total ?? 0 };
        st.dones = { items: 0, bytes: 0 };
        st.samples = [{ t: activeNow(st), b: 0, i: 0 }];
        st.currentPath = '';
        break;
      }
      case 'totals':
        if (ev.phase === st.phase) {
          st.totals = { items: ev.items_total ?? 0, bytes: ev.bytes_total ?? 0 };
          st.dones = ev.reset
            ? { items: ev.items_done ?? 0, bytes: ev.bytes_done ?? 0 }
            : {
                items: Math.max(st.dones.items, ev.items_done ?? 0),
                bytes: Math.max(st.dones.bytes, ev.bytes_done ?? 0),
              };
          const sample = { t: activeNow(st), b: st.dones.bytes, i: st.dones.items };
          if (ev.reset) st.samples = [sample];
          else st.samples.push(sample);
        }
        break;
      case 'progress': {
        let row = st.stages.find((x) => x.phase === ev.phase);
        if (!row) { row = { phase: ev.phase!, detail: '', active: true, done: false }; st.stages.push(row); }
        const it = ev.items_total ?? 0, bt = ev.bytes_total ?? 0;
        row.detail = `${ev.items_done ?? 0}${it ? ` / ${it}` : ''} items · ${humanSize(ev.bytes_done ?? 0)}${bt ? ` / ${humanSize(bt)}` : ''}`;
        if (ev.phase === st.phase) {
          // Worker events can reach the webview in a different order. Totals is the explicit epoch
          // reset (walk → hash); inside an epoch, counters never regress.
          st.dones = {
            items: Math.max(st.dones.items, ev.items_done ?? 0),
            bytes: Math.max(st.dones.bytes, ev.bytes_done ?? 0),
          };
          st.totals = { items: ev.items_total ?? st.totals.items, bytes: ev.bytes_total ?? st.totals.bytes };
          st.currentPath = ev.current_path ?? st.currentPath;
          st.samples.push({ t: activeNow(st), b: st.dones.bytes, i: st.dones.items } as Sample);
          if (st.samples.length > 4000) st.samples.splice(0, 1000);
        }
        break;
      }
      case 'phase_end': {
        const row = st.stages.find((x) => x.phase === ev.phase);
        if (row) {
          endStage(row, ev.status);
          const it = ev.items_total ?? 0, bt = ev.bytes_total ?? 0;
          row.detail = `${ev.items_done ?? 0}${it ? ` / ${it}` : ''} items · ${humanSize(ev.bytes_done ?? 0)}${bt ? ` / ${humanSize(bt)}` : ''}`;
        }
        if (ev.phase === st.phase) {
          st.dones = { items: ev.items_done ?? st.dones.items, bytes: ev.bytes_done ?? st.dones.bytes };
          st.totals = { items: ev.items_total ?? st.totals.items, bytes: ev.bytes_total ?? st.totals.bytes };
          st.samples.push({ t: activeNow(st), b: st.dones.bytes, i: st.dones.items });
        }
        break;
      }
      case 'error':
        st.errors.push({
          path: ev.path ?? '', action: ev.action ?? '', side: ev.side ?? '',
          message: ev.message ?? '', warning: ev.action === 'warning',
        });
        rerender();
        break;
      // v0.10: pipeline narrative. info stays out of this panel (it would drown the real problems), only
      // warn/error get in — they used to go to stderr only, which in a windowed build means never said at all.
      case 'log':
        if (ev.level === 'warn' || ev.level === 'error') {
          st.errors.push({
            path: '', action: ev.scope ?? 'log', side: '',
            message: ev.message ?? '', warning: ev.level === 'warn',
          });
          rerender();
        }
        break;
      case 'paused':
        st.pausedSince = Date.now();
        rerender();
        break;
      case 'resumed':
        st.pausedSince = 0;
        st.pausedMs = ev.paused_ms ?? st.pausedMs;
        rerender();
        break;
      case 'summary': {
        pendingLaunch.current = null;
        st.summary = ev;
        st.running = false;
        st.pausedSince = 0;
        st.pausedMs = ev.paused_ms ?? st.pausedMs;
        // push dones to completion in the final state (throttling can swallow the last Progress event)
        if (!ev.cancelled && (ev.errors ?? 0) === 0 && st.totals.bytes + st.totals.items > 0) {
          st.dones = { items: st.totals.items, bytes: st.totals.bytes };
          st.samples.push({ t: activeNow(st), b: st.dones.bytes, i: st.dones.items });
        }
        for (const row of st.stages) row.active = false;
        const cur = st.stages.find((x) => x.phase === st.phase);
        if (cur && !cur.done && !cur.failed && !cur.cancelled) {
          cur.cancelled = !!ev.cancelled;
          cur.failed = !ev.cancelled;
        }
        rerender();
        if (st.closeAfterStop) { win.close().catch(() => {}); break; }
        if (ev.cancelled) break;
        if ((ev.errors ?? 0) === 0 && autocloseRef.current && st.applying) {
          setTimeout(() => { if (run.current.summary) win.close().catch(() => {}); }, 1200);
          break;
        }
        if ((ev.errors ?? 0) === 0 && whenFinRef.current !== 'none' && st.applying) {
          setCountdown({ kind: whenFinRef.current, left: 10 });
        }
        break;
      }
    }
  }, [rerender]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let ready = false;
    let lastSequence = 0;
    const queued: RunEv[] = [];
    const publish = (event: RunEv) => {
      if (!ready) {
        queued.push(event);
        return;
      }
      if (event.sequence <= lastSequence) return;
      lastSequence = event.sequence;
      onEvent(event);
    };
    void Promise.all([
      listen<RunEv>('run-progress', (e) => publish(e.payload)),
      listen<RunRejected>('run-rejected', (e) => {
        const pending = pendingLaunch.current;
        if (!pending || e.payload.launch_id !== pending.id) return;
        pendingLaunch.current = null;
        const closeAfterStop = run.current.closeAfterStop;
        const reset = newRunState();
        reset.runId = pending.afterRunId;
        run.current = reset;
        setRejected(e.payload.message);
        setStop('finished');
        rerender();
        if (closeAfterStop) destroyProgressWindow().catch(() => {});
      }),
      listen<number>('progress-window-arm', (e) => armLaunch(e.payload)),
    ]).then(async (stops) => {
      if (disposed) {
        for (const stopListening of stops) stopListening();
        return;
      }
      unlisten = () => { for (const stopListening of stops) stopListening(); };
      try {
        const replay = await replayRunEvents('apply');
        if (!disposed) {
          const pending = mergeRunEventReplay(replay, queued);
          queued.length = 0;
          ready = true;
          for (const event of pending) publish(event);
        }
      } catch (error) {
        if (disposed) return;
        ready = true;
        for (const event of queued.splice(0)) publish(event);
        reportControlError('Could not restore progress after reconnect', error);
      }
      if (disposed) return;
      await emit('progress-window-mounted');
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [armLaunch, onEvent, reportControlError, rerender]);

  // The numeric readouts step every 500ms (same divider as FFS — figures that flip ten times a second
  // are unreadable). The graphs keep their own 100ms repaint inside <Graph>, off the React tree.
  useEffect(() => {
    const id = window.setInterval(() => { if (run.current.runId >= 0) rerender(); }, 500);
    return () => window.clearInterval(id);
  }, [rerender]);

  // Window title + taskbar (fails silently: macOS has no taskbar progress)
  const s = run.current;
  const pct = percent(s);
  const paused = s.pausedSince > 0;
  useEffect(() => {
    const title = s.summary
      ? (s.summary.cancelled ? 'Stopped — SyncDash' : 'Done — SyncDash')
      : rejected ? 'Could not start — SyncDash'
      : s.applying ? `${Math.round(pct)}% — SyncDash` : `${s.phase ? PHASE_LABEL[s.phase] : 'Running'} — SyncDash`;
    win.setTitle(title).catch(() => {});
    if (s.summary) win.setProgressBar({ status: ProgressBarStatus.None }).catch(() => {});
    else if (s.applying) {
      win.setProgressBar({
        status: paused ? ProgressBarStatus.Paused : ProgressBarStatus.Normal,
        progress: Math.round(pct),
      }).catch(() => {});
    }
  }, [paused, pct, rejected, s.applying, s.phase, s.summary]);

  // Close button = FFS semantics: while running, cooperatively cancel first and leave once Summary arrives
  useEffect(() => {
    const un = win.onCloseRequested(async (e) => {
      e.preventDefault();
      // React's view may still describe the previous run while a new launch is being armed. Ask
      // the backend first: clearing a pending reservation or cancelling the matching interactive
      // run is atomic with begin_run_for_launch, so a close can never start a headless sync.
      run.current.closeAfterStop = true;
      let launchState: Awaited<ReturnType<typeof closeProgressLaunch>>;
      try { launchState = await closeProgressLaunch(); }
      catch (error) { reportControlError('Could not inspect the active launch before closing', error); return; }
      let current = run.current;
      current.closeAfterStop = true;
      if (launchState === 'pending') {
        pendingLaunch.current = null;
        destroyProgressWindow().catch(() => {});
        return;
      }
      if (launchState === 'active') {
        if (current.summary) destroyProgressWindow().catch(() => {});
        else setStop('stopping');
        return;
      }

      // No interactive launch is pending/active. The window can still be showing an unattended
      // apply; retain the prior cooperative-cancel behavior for that case.
      if (current.running && !current.summary) {
        setStop('stopping');
        if (current.runId < 0) return;
        if (current.pausedSince) {
          current.pausedSince = 0;
          try { await pauseRun(current.runId, false); }
          catch (error) { reportControlError('Could not resume before stopping', error); return; }
        }
        let had: boolean;
        try { had = await cancelRun(current.runId); }
        catch (error) { reportControlError('Could not stop this run', error); return; }
        if (!had) {
          // Catch a launch reserved while cancelRun was in flight before destroying the window.
          let racedLaunch: Awaited<ReturnType<typeof closeProgressLaunch>>;
          try { racedLaunch = await closeProgressLaunch(); }
          catch (error) { reportControlError('Could not inspect a raced launch before closing', error); return; }
          current = run.current;
          current.closeAfterStop = true;
          if (racedLaunch === 'active') {
            setStop('stopping');
            return;
          }
          if (racedLaunch === 'pending') pendingLaunch.current = null;
        } else {
          return;
        }
      }
      destroyProgressWindow().catch(() => {});
    });
    return () => { un.then((f) => f()); };
  }, [reportControlError]);

  const r60 = windowRate(s, 60000);
  const nErr = s.errors.filter((e) => !e.warning).length;
  const nWarn = s.errors.length - nErr;

  const countersExhausted = s.totals.items + s.totals.bytes > 0
    && (s.totals.items === 0 || s.dones.items >= s.totals.items)
    && (s.totals.bytes === 0 || s.dones.bytes >= s.totals.bytes);
  // The copy loop reports its final byte before seal/fsync/verify/preserve/commit completes the
  // item. On an external mirror, preservation may itself be a long cross-volume copy.
  const finalizing = countersExhausted
    || (s.totals.bytes > 0 && s.dones.bytes >= s.totals.bytes);
  const eta = (() => {
    if (s.summary) return '—';
    if (finalizing) return 'Finalizing…';
    if (r60 && s.totals.bytes > 0 && r60.bps > 1) return humanDuration(Math.max(0, s.totals.bytes - s.dones.bytes) / r60.bps * 1000) + ' remaining';
    if (r60 && s.totals.items > 0 && r60.ips > 0.01) return humanDuration(Math.max(0, s.totals.items - s.dones.items) / r60.ips * 1000) + ' remaining';
    return 'Estimating…';
  })();

  return (
    <div className={'pwin' + (s.applying ? ' applying' : '')}>
      <div className="phead">
        <span className="pjob">SyncDash</span>
        <span className="pphase">
          {s.summary
            ? (s.summary.cancelled
              ? `Cancelled — ${s.summary.done} applied, ${s.summary.skipped} skipped`
              : `Done — ${s.summary.done} applied, ${s.summary.skipped} skipped, ${s.summary.errors} errors · ${humanSize(s.summary.bytes_done ?? 0)} · ${humanDuration(s.summary.elapsed_ms ?? 0)}`)
            : rejected ? `Could not start — ${rejected}`
            : s.running
              ? <>{paused && <><Pause size={12} /> Paused — </>}{s.phase ? PHASE_LABEL[s.phase] : ''}{finalizing ? ' — Finalizing' : ''}</>
              : 'Waiting for a run…'}
        </span>
        <span
          className={'ppct ' + (s.summary ? (s.summary.cancelled || s.summary.errors ? 'err' : 'ok') : paused ? 'paused' : '')}
        >
          {s.summary
            ? (s.summary.cancelled ? 'Stopped' : `${Math.round(pct)}%`)
            : rejected ? 'Failed'
            : s.running ? (s.applying ? `${Math.round(pct)}%` : '…') : '—'}
        </span>
      </div>

      <div className="stagerows">
        {s.stages.map((row) => (
          <div key={row.phase} className={'stagerow' + (row.active ? ' active' : '') + (row.done ? ' done' : '')}>
            <span className="st-ico">
              {row.active ? <RefreshCw size={13} className="spin" />
                : row.failed ? <TriangleAlert size={13} className="icon-err" />
                  : row.cancelled ? <Square size={12} />
                    : row.done ? <Check size={13} />
                      : <RefreshCw size={13} />}
            </span>
            <span className="st-name">{PHASE_LABEL[row.phase]}</span>
            <span className="st-detail">{row.detail}</span>
          </div>
        ))}
      </div>

      <div className="graphs">
        <Graph caption="Data (cumulative bytes)" field="b" runRef={run} rateText={rateBytes} />
        <Graph caption="Items (cumulative count)" field="i" runRef={run} rateText={rateItems} />
        <div className="readouts">
          <div className="rh" /><div className="rh">Processed</div><div className="rh">Remaining</div>
          <div className="rh">Items</div>
          <div>{s.dones.items} / {s.totals.items}</div>
          <div>{Math.max(0, s.totals.items - s.dones.items)}</div>
          <div className="rh">Bytes</div>
          <div>{humanSize(s.dones.bytes)} / {humanSize(s.totals.bytes)}</div>
          <div>{humanSize(Math.max(0, s.totals.bytes - s.dones.bytes))}</div>
          <div className="rh">Time</div>
          <div>{humanDuration(s.summary ? s.summary.elapsed_ms ?? 0 : activeNow(s))}</div>
          <div>{eta}</div>
        </div>
        <div className="curfile" title={s.currentPath}>{s.currentPath ? `‎${s.currentPath}` : ''}</div>
      </div>

      <div className={'errsec' + (s.errors.length ? ' show' : '') + (errorsOpen ? ' open' : '')}>
        <div className="errhead" onClick={() => setErrorsOpen((v) => !v)}>
          <TriangleAlert size={14} className={nErr ? 'icon-err' : 'icon-warn'} />
          <span className="cnt-err">{nErr ? `${nErr} errors` : ''}</span>
          <span className="cnt-warn">{nWarn ? `${nWarn} warnings` : ''}</span>
          <span className="dim errtip">Click to expand</span>
        </div>
        <div className="errlist">
          {s.errors.map((e, k) => (
            <div key={k} className={'erow' + (e.warning ? ' warn' : '')}>
              <span className="epath mono">{e.path}</span>{' '}
              {/* narrative entries (kind=log) have no path/side — don't render an empty slot like "[scope/] msg" */}
              <span className="emsg">{e.side ? `[${e.action}/${e.side}] ` : `[${e.action}] `}{e.message}</span>
            </div>
          ))}
        </div>
      </div>

      {countdown && (
        <div className="countdown show">
          <span className="cdtext">{countdown.kind === 'sleep' ? 'Sleep' : 'Shut down'} in {countdown.left}s</span>
          <div className="cdbar"><div className="cdfill" style={{ width: `${countdown.left * 10}%` }} /></div>
          <button className="btn" onClick={() => setCountdown(null)}>Cancel</button>
        </div>
      )}

      <div className="controls">
        <button
          className="btn"
          disabled={!s.running || !!s.summary}
          onClick={async () => {
            const wantPause = run.current.pausedSince === 0;
            const previousPausedSince = run.current.pausedSince;
            // optimistic toggle, corrected once the event arrives
            run.current.pausedSince = wantPause ? Date.now() : 0;
            rerender();
            try {
              const accepted = await pauseRun(run.current.runId, wantPause);
              if (!accepted) throw new Error('the run already finished');
            }
            catch (error) {
              run.current.pausedSince = previousPausedSince;
              reportControlError(wantPause ? 'Could not pause this run' : 'Could not resume this run', error);
            }
          }}
        >{paused ? <><Play size={12} /> Continue</> : <><Pause size={12} /> Pause</>}</button>
        <button
          className="btn btn-stop"
          disabled={!s.running || !!s.summary || stop !== 'idle'}
          onClick={async () => {
            setStop('stopping');
            const st = run.current;
            // Stop pressed while paused: resume first, otherwise the cancel never reaches a checkpoint
            if (st.pausedSince) {
              st.pausedSince = 0;
              try { await pauseRun(st.runId, false); }
              catch (error) { reportControlError('Could not resume before stopping', error); return; }
            }
            let had: boolean;
            try { had = await cancelRun(st.runId); }
            catch (error) { reportControlError('Could not stop this run', error); return; }
            // no active run (it already finished on its own) — don't leave the button stuck on "Stopping…"
            if (!had) setStop('finished');
          }}
        >
          {stop === 'idle' ? <><Square size={12} /> Stop</> : stop === 'stopping' ? 'Stopping…' : 'Finished'}
        </button>
        <label className="dim chkline">
          <input
            type="checkbox"
            checked={autoclose}
            onChange={(e) => { setAutoclose(e.target.checked); localStorage.setItem('sd.autoclose', e.target.checked ? '1' : '0'); }}
          /> Auto-close when finished
        </label>
        <span className="wfin">
          When finished
          <select
            value={whenFin}
            onChange={(e) => { setWhenFin(e.target.value); localStorage.setItem('sd.whenfin', e.target.value); }}
          >
            <option value="none">Do nothing</option>
            <option value="sleep">Sleep</option>
            <option value="shutdown">Shut down</option>
          </select>
        </span>
      </div>
    </div>
  );
}
