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
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, ProgressBarStatus } from '@tauri-apps/api/window';
import { cancelRun, pauseRun, postSyncAction } from '../core/ipc';
import { humanDuration, humanSize } from '../core/format';
import { applyZoom, readZoom } from '../core/zoom';
import { Graph } from './Graph';
import type { RunEv, RunState, Sample } from './runstate';
import { PHASE_LABEL, activeNow, newRunState, percent, windowRate } from './runstate';

const win = getCurrentWindow();

export function ProgressApp() {
  const run = useRef<RunState>(newRunState());
  // Bumping this is the only thing that re-renders; the tick below decides how often that happens
  const [, forceRender] = useState(0);
  const rerender = useCallback(() => forceRender((n) => n + 1), []);

  const [autoclose, setAutoclose] = useState(() => localStorage.getItem('sd.autoclose') === '1');
  const [whenFin, setWhenFin] = useState(() => localStorage.getItem('sd.whenfin') ?? 'none');
  const [countdown, setCountdown] = useState<{ kind: string; left: number } | null>(null);
  const [errorsOpen, setErrorsOpen] = useState(false);
  const [stopLabel, setStopLabel] = useState('■ Stop');

  const autocloseRef = useRef(autoclose);
  autocloseRef.current = autoclose;
  const whenFinRef = useRef(whenFin);
  whenFinRef.current = whenFin;

  useEffect(() => {
    if (navigator.userAgent.includes('Macintosh')) document.body.classList.add('mac');
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
    if (ev.run_id < s.runId) return;        // late event from an already-cancelled run
    if (ev.run_id > s.runId) {
      run.current = newRunState(ev.run_id, ev.ts_ms);
      setStopLabel('■ Stop');
      setCountdown(null);
    }
    const st = run.current;

    switch (ev.kind) {
      case 'phase_start': {
        const p = ev.phase!;
        // tick off the previous phase's row
        if (st.phase && st.phase !== p) {
          const prev = st.stages.find((x) => x.phase === st.phase);
          if (prev) { prev.active = false; prev.done = true; }
        }
        st.phase = p;
        let row = st.stages.find((x) => x.phase === p);
        if (!row) { row = { phase: p, detail: '', active: true, done: false }; st.stages.push(row); }
        row.active = true;
        row.done = false;
        if (ev.label) row.detail = ev.label;
        if (p === 'apply' || p === 'pack' || p === 'ship') st.applying = true;
        if (p === 'apply') {
          st.totals = { items: ev.items_total ?? 0, bytes: ev.bytes_total ?? 0 };
          st.dones = { items: 0, bytes: 0 };
          st.samples = [{ t: activeNow(st), b: 0, i: 0 }];
        }
        break;
      }
      case 'totals':
        if (ev.phase === st.phase) st.totals = { items: ev.items_total ?? 0, bytes: ev.bytes_total ?? 0 };
        break;
      case 'progress': {
        let row = st.stages.find((x) => x.phase === ev.phase);
        if (!row) { row = { phase: ev.phase!, detail: '', active: true, done: false }; st.stages.push(row); }
        const it = ev.items_total ?? 0, bt = ev.bytes_total ?? 0;
        row.detail = `${ev.items_done ?? 0}${it ? ` / ${it}` : ''} items · ${humanSize(ev.bytes_done ?? 0)}${bt ? ` / ${humanSize(bt)}` : ''}`;
        if (ev.phase === 'apply') {
          st.dones = { items: ev.items_done ?? 0, bytes: ev.bytes_done ?? 0 };
          st.totals = { items: ev.items_total ?? st.totals.items, bytes: ev.bytes_total ?? st.totals.bytes };
          st.currentPath = ev.current_path ?? st.currentPath;
          st.samples.push({ t: activeNow(st), b: st.dones.bytes, i: st.dones.items } as Sample);
          if (st.samples.length > 4000) st.samples.splice(0, 1000);
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
        st.summary = ev;
        st.running = false;
        st.pausedSince = 0;
        st.pausedMs = ev.paused_ms ?? st.pausedMs;
        // push dones to completion in the final state (throttling can swallow the last Progress event)
        if (!ev.cancelled && (ev.errors ?? 0) === 0 && st.totals.bytes + st.totals.items > 0) {
          st.dones = { items: st.totals.items, bytes: st.totals.bytes };
          st.samples.push({ t: activeNow(st), b: st.dones.bytes, i: st.dones.items });
        }
        const cur = st.stages.find((x) => x.phase === st.phase);
        if (cur) { cur.active = false; cur.done = true; cur.cancelled = !!ev.cancelled; }
        rerender();
        if (st.closeAfterStop) { win.close().catch(() => {}); break; }
        if (ev.cancelled) break;
        if ((ev.errors ?? 0) === 0 && autocloseRef.current && st.applying) {
          setTimeout(() => { if (run.current.summary) win.close().catch(() => {}); }, 1200);
          break;
        }
        if (whenFinRef.current !== 'none' && st.applying) setCountdown({ kind: whenFinRef.current, left: 10 });
        break;
      }
    }
  }, [rerender]);

  useEffect(() => {
    const un = listen<RunEv>('run-progress', (e) => onEvent(e.payload));
    return () => { un.then((f) => f()); };
  }, [onEvent]);

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
      : s.applying ? `${Math.round(pct)}% — SyncDash` : `${s.phase ? PHASE_LABEL[s.phase] : 'Running'} — SyncDash`;
    win.setTitle(title).catch(() => {});
    if (s.summary) win.setProgressBar({ status: ProgressBarStatus.None }).catch(() => {});
    else if (s.applying) {
      win.setProgressBar({
        status: paused ? ProgressBarStatus.Paused : ProgressBarStatus.Normal,
        progress: Math.round(pct),
      }).catch(() => {});
    }
  });

  // Close button = FFS semantics: while running, cooperatively cancel first and leave once Summary arrives
  useEffect(() => {
    const un = win.onCloseRequested(async (e) => {
      const st = run.current;
      if (st.running && !st.summary) {
        e.preventDefault();
        st.closeAfterStop = true;
        setStopLabel('Stopping…');
        if (st.pausedSince) { st.pausedSince = 0; await pauseRun(false).catch(() => {}); }
        const had = await cancelRun().catch(() => false);
        if (!had) {
          // nothing running on the engine side (Summary lost / already finished) — let the close through;
          // never leave the window unclosable
          st.running = false;
          win.destroy().catch(() => {});
        }
      }
    });
    return () => { un.then((f) => f()); };
  }, []);

  const r60 = windowRate(s, 60000);
  const nErr = s.errors.filter((e) => !e.warning).length;
  const nWarn = s.errors.length - nErr;

  const eta = (() => {
    if (s.summary) return '—';
    if (r60 && s.totals.bytes > 0 && r60.bps > 1) return humanDuration(Math.max(0, s.totals.bytes - s.dones.bytes) / r60.bps * 1000) + ' remaining';
    if (r60 && s.totals.items > 0 && r60.ips > 0.01) return humanDuration(Math.max(0, s.totals.items - s.dones.items) / r60.ips * 1000) + ' remaining';
    return 'Estimating…';
  })();

  return (
    <div id="pwin" className={s.applying ? 'applying' : ''}>
      <div id="phead">
        <span id="pjob">SyncDash</span>
        <span id="pphase">
          {s.summary
            ? (s.summary.cancelled
              ? `Cancelled — ${s.summary.done} applied, ${s.summary.skipped} skipped`
              : `Done — ${s.summary.done} applied, ${s.summary.skipped} skipped, ${s.summary.errors} errors · ${humanSize(s.summary.bytes_done ?? 0)} · ${humanDuration(s.summary.elapsed_ms ?? 0)}`)
            : s.running
              ? (paused ? '⏸ Paused — ' : '') + (s.phase ? PHASE_LABEL[s.phase] : '')
              : 'Waiting for a run…'}
        </span>
        <span
          id="ppct"
          className={s.summary ? (s.summary.cancelled || s.summary.errors ? 'err' : 'ok') : paused ? 'paused' : ''}
        >
          {s.summary
            ? (s.summary.cancelled ? 'Stopped' : `${Math.round(pct)}%`)
            : s.running ? (s.applying ? `${Math.round(pct)}%` : '…') : '—'}
        </span>
      </div>

      <div id="stagerows">
        {s.stages.map((row) => (
          <div key={row.phase} className={'stagerow' + (row.active ? ' active' : '') + (row.done ? ' done' : '')}>
            <span className="st-ico">{row.done ? (row.cancelled ? '■' : '✓') : '⟳'}</span>
            <span className="st-name">{PHASE_LABEL[row.phase]}</span>
            <span className="st-detail">{row.detail}</span>
          </div>
        ))}
      </div>

      <div id="graphs">
        <Graph caption="Data (cumulative bytes)" field="b" runRef={run} rateText={rateBytes} />
        <Graph caption="Items (cumulative count)" field="i" runRef={run} rateText={rateItems} />
        <div id="readouts">
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
        <div id="curfile" title={s.currentPath}>{s.currentPath ? `‎${s.currentPath}` : ''}</div>
      </div>

      <div id="errsec" className={(s.errors.length ? 'show' : '') + (errorsOpen ? ' open' : '')}>
        <div id="errhead" onClick={() => setErrorsOpen((v) => !v)}>
          <span>⚠</span>
          <span className="cnt-err">{nErr ? `${nErr} errors` : ''}</span>
          <span className="cnt-warn">{nWarn ? `${nWarn} warnings` : ''}</span>
          <span className="dim errtip">Click to expand</span>
        </div>
        <div id="errlist">
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
        <div id="countdown" className="show">
          <span id="cdtext">{countdown.kind === 'sleep' ? 'Sleep' : 'Shut down'} in {countdown.left}s</span>
          <div id="cdbar"><div id="cdfill" style={{ width: `${countdown.left * 10}%` }} /></div>
          <button className="btn" onClick={() => setCountdown(null)}>Cancel</button>
        </div>
      )}

      <div id="controls">
        <button
          className="btn"
          disabled={!s.running || !!s.summary}
          onClick={async () => {
            const wantPause = run.current.pausedSince === 0;
            // optimistic toggle, corrected once the event arrives
            run.current.pausedSince = wantPause ? Date.now() : 0;
            rerender();
            await pauseRun(wantPause).catch(() => {});
          }}
        >{paused ? '▶ Continue' : '⏸ Pause'}</button>
        <button
          id="btn-stop"
          className="btn"
          disabled={!s.running || !!s.summary || stopLabel !== '■ Stop'}
          onClick={async () => {
            setStopLabel('Stopping…');
            const st = run.current;
            // Stop pressed while paused: resume first, otherwise the cancel never reaches a checkpoint
            if (st.pausedSince) { st.pausedSince = 0; await pauseRun(false).catch(() => {}); }
            const had = await cancelRun().catch(() => false);
            // no active run (it already finished on its own) — don't leave the button stuck on "Stopping…"
            if (!had) setStopLabel('Finished');
          }}
        >{stopLabel}</button>
        <label className="dim chkline">
          <input
            type="checkbox"
            checked={autoclose}
            onChange={(e) => { setAutoclose(e.target.checked); localStorage.setItem('sd.autoclose', e.target.checked ? '1' : '0'); }}
          /> Auto-close when finished
        </label>
        <span id="wfin">
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
