// SyncDash progress sub-window (v0.9 M2, FFS-equivalent behavior parameters):
// - rate = 4s sliding window, ETA = 60s sliding window, readouts refresh every 500ms, redraw every 100ms
// - percentage = (bytesDone + itemsDone) / (bytesTotal + itemsTotal) (same formula as FFS)
// - paused time is excluded from elapsed; Stop = cooperative cancel (stops after the current chunk, so the end state never leaves a half-written file)
// - errors accumulate without interrupting; Auto-close and When-finished (sleep/shut down, 10s cancellable countdown)
// Data source: the `run-progress` event broadcast by the Rust TauriSink (throttled to ≥100ms apiece, carries run_id).

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, ProgressBarStatus } from '@tauri-apps/api/window';

type PhaseName =
  | 'scan-source' | 'scan-target' | 'compare' | 'apply'
  | 'pack' | 'ship' | 'verify' | 'refresh';

interface RunEv {
  run_id: number;
  /// "compare" | "apply" — this window only accepts apply (compare progress is shown inline in the main window;
  /// without the filter, the automatic re-check compare after a sync would hijack the result window into a forever-spinning "comparing")
  purpose?: string;
  kind: 'phase_start' | 'totals' | 'progress' | 'error' | 'log' | 'item_result' | 'paused' | 'resumed' | 'summary';
  /// kind='log' only: info | warn | error
  level?: 'info' | 'warn' | 'error';
  /// kind='log' only: module name (run / pack / lock…)
  scope?: string;
  ts_ms: number;
  phase?: PhaseName;
  label?: string | null;
  items_total?: number;
  bytes_total?: number;
  items_done?: number;
  bytes_done?: number;
  current_path?: string;
  path?: string;
  action?: string;
  side?: string;
  message?: string;
  paused_ms?: number;
  done?: number;
  skipped?: number;
  errors?: number;
  elapsed_ms?: number;
  cancelled?: boolean;
}

const PHASE_LABEL: Record<PhaseName, string> = {
  'scan-source': 'Scan source',
  'scan-target': 'Scan target',
  'compare': 'Compare',
  'apply': 'Sync',
  'pack': 'Pack',
  'ship': 'Transfer',
  'verify': 'Verify',
  'refresh': 'Refresh archive',
};

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const win = getCurrentWindow();

// Run state

interface Sample { t: number; b: number; i: number } // t = active milliseconds (paused time removed)

let runId = -1;
let running = false;
let t0 = 0;                       // ts of the first event in this run
let pausedMs = 0;                 // authoritative total from the engine (refreshed by Resumed/Summary)
let pausedSince = 0;              // start of the current pause (local estimate)
let phase: PhaseName | null = null;
let applying = false;            // has entered apply/pack/ship → graph mode
let totals = { items: 0, bytes: 0 };
let dones = { items: 0, bytes: 0 };
let samples: Sample[] = [];
let currentPath = '';
let errors: { path: string; action: string; side: string; message: string; warning: boolean }[] = [];
let summary: RunEv | null = null;
let closeAfterStop = false;

function activeNow(): number {
  const now = Date.now();
  const livePause = pausedSince ? now - pausedSince : 0;
  return Math.max(0, now - t0 - pausedMs - livePause);
}

function resetRun(id: number, ts: number) {
  runId = id;
  running = true;
  t0 = ts;
  pausedMs = 0; pausedSince = 0;
  phase = null; applying = false;
  totals = { items: 0, bytes: 0 };
  dones = { items: 0, bytes: 0 };
  samples = [];
  currentPath = '';
  errors = [];
  summary = null;
  closeAfterStop = false;
  stageEl.innerHTML = '';
  stageRows.clear();
  errList.innerHTML = '';
  errSec.classList.remove('show', 'open');
  countdownStop();
  pwin.classList.remove('applying');
  btnPause.disabled = false;
  btnPause.textContent = '⏸ Pause';
  btnStop.disabled = false;
  btnStop.textContent = '■ Stop';
  pctEl.className = '';
  renderReadouts();
}

// DOM

const pwin = $('pwin');
const jobEl = $('pjob');
const phaseEl = $('pphase');
const pctEl = $('ppct');
const stageEl = $('stagerows');
const rateBytesEl = $('rate-bytes');
const rateItemsEl = $('rate-items');
const curfileEl = $('curfile');
const errSec = $('errsec');
const errList = $('errlist');
const cntErr = $('cnt-err');
const cntWarn = $('cnt-warn');
const btnPause = $<HTMLButtonElement>('btn-pause');
const btnStop = $<HTMLButtonElement>('btn-stop');
const chkAutoclose = $<HTMLInputElement>('chk-autoclose');
const selWfin = $<HTMLSelectElement>('sel-wfin');
const cdEl = $('countdown');
const cdText = $('cdtext');
const cdFill = $('cdfill');

const stageRows = new Map<PhaseName, { row: HTMLElement; detail: HTMLElement; ico: HTMLElement }>();

function stageRow(p: PhaseName): { row: HTMLElement; detail: HTMLElement; ico: HTMLElement } {
  let r = stageRows.get(p);
  if (r) return r;
  const row = document.createElement('div');
  row.className = 'stagerow';
  const ico = document.createElement('span');
  ico.className = 'st-ico';
  ico.textContent = '⟳';
  const name = document.createElement('span');
  name.className = 'st-name';
  name.textContent = PHASE_LABEL[p];
  const detail = document.createElement('span');
  detail.className = 'st-detail';
  row.append(ico, name, detail);
  stageEl.appendChild(row);
  r = { row, detail, ico };
  stageRows.set(p, r);
  return r;
}

function hb(n: number): string {
  if (n >= 1 << 30) return (n / (1 << 30)).toFixed(2) + ' GB';
  if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + ' MB';
  if (n >= 1024) return (n / 1024).toFixed(1) + ' KB';
  return n + ' B';
}
function ht(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), ss = s % 60;
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${String(ss).padStart(2, '0')}` : `${m}:${String(ss).padStart(2, '0')}`;
}

// Rate / ETA (differencing the sample series over a sliding window)

function windowRate(windowMs: number): { bps: number; ips: number } | null {
  if (samples.length < 2) return null;
  const last = samples[samples.length - 1];
  const cut = last.t - windowMs;
  // walk back from the tail for the first sample <= cut; if the oldest sample is younger than the window, use it (same as FFS: "when the window is not full, use the actual span")
  let base = samples[0];
  for (let k = samples.length - 2; k >= 0; k--) {
    if (samples[k].t <= cut) { base = samples[k]; break; }
  }
  const dt = last.t - base.t;
  if (dt < 200) return null;
  return { bps: (last.b - base.b) * 1000 / dt, ips: (last.i - base.i) * 1000 / dt };
}

function percent(): number {
  const denom = totals.bytes + totals.items;
  if (denom <= 0) return 0;
  return Math.min(100, (dones.bytes + dones.items) * 100 / denom);
}

// Graphs

function drawGraph(cv: HTMLCanvasElement, key: 'b' | 'i', total: number) {
  const ctx = cv.getContext('2d');
  if (!ctx) return;
  const dpr = window.devicePixelRatio || 1;
  const w = cv.clientWidth, h = cv.clientHeight;
  if (cv.width !== w * dpr || cv.height !== h * dpr) { cv.width = w * dpr; cv.height = h * dpr; }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const css = getComputedStyle(document.documentElement);
  const cAccent = css.getPropertyValue('--accent').trim() || '#3a8fd9';
  const cDim = css.getPropertyValue('--dim').trim() || '#7b8290';
  const cBorder = css.getPropertyValue('--border').trim() || '#2c313c';

  const now = activeNow();
  const lastV = samples.length ? samples[samples.length - 1][key] : 0;
  // the ETA endpoint takes part in the x axis: the completion estimate sits at the right edge (a simplified FFS wedge: a dashed marker)
  const r = windowRate(60000);
  const rate = key === 'b' ? r?.bps ?? 0 : r?.ips ?? 0;
  const remain = Math.max(0, total - lastV);
  const etaMs = rate > 1e-6 && total > 0 ? remain / rate * 1000 : 0;
  const xMax = Math.max(now + etaMs, now, 1000);
  const yMax = Math.max(total, lastV, 1);
  const X = (t: number) => t / xMax * (w - 8) + 4;
  const Y = (v: number) => h - 6 - v / yMax * (h - 16);

  // grid
  ctx.strokeStyle = cBorder; ctx.lineWidth = 1;
  for (let g = 1; g <= 3; g++) {
    const y = 6 + (h - 12) * g / 4;
    ctx.beginPath(); ctx.moveTo(2, y); ctx.lineTo(w - 2, y); ctx.stroke();
  }
  // dashed line for the total
  if (total > 0) {
    ctx.setLineDash([4, 3]); ctx.strokeStyle = cDim;
    ctx.beginPath(); ctx.moveTo(2, Y(total)); ctx.lineTo(w - 2, Y(total)); ctx.stroke();
    ctx.setLineDash([]);
  }
  // cumulative curve + fill (decimated by a step once there are >300 samples)
  if (samples.length > 1) {
    const step = Math.max(1, Math.floor(samples.length / 300));
    ctx.beginPath(); ctx.moveTo(X(0), Y(0));
    for (let k = 0; k < samples.length; k += step) ctx.lineTo(X(samples[k].t), Y(samples[k][key]));
    ctx.lineTo(X(samples[samples.length - 1].t), Y(lastV));
    ctx.strokeStyle = cAccent; ctx.lineWidth = 1.5; ctx.stroke();
    ctx.lineTo(X(samples[samples.length - 1].t), Y(0)); ctx.closePath();
    ctx.fillStyle = cAccent + '33'; ctx.fill();
  }
  // "now" vertical line + estimated-completion dashed line
  ctx.strokeStyle = cDim;
  ctx.beginPath(); ctx.moveTo(X(now), 4); ctx.lineTo(X(now), h - 4); ctx.stroke();
  if (etaMs > 0) {
    ctx.setLineDash([3, 3]);
    ctx.beginPath(); ctx.moveTo(X(now + etaMs), 4); ctx.lineTo(X(now + etaMs), h - 4); ctx.stroke();
    ctx.setLineDash([]);
  }
}

// Render

let lastReadout = 0;

function renderReadouts() {
  const pct = percent();
  const paused = pausedSince > 0;
  if (summary) {
    const s = summary;
    pctEl.textContent = s.cancelled ? 'Stopped' : `${Math.round(percent())}%`;
    pctEl.className = s.cancelled ? 'err' : (s.errors ? 'err' : 'ok');
    phaseEl.textContent = s.cancelled
      ? `Cancelled — ${s.done} applied, ${s.skipped} skipped`
      : `Done — ${s.done} applied, ${s.skipped} skipped, ${s.errors} errors · ${hb(s.bytes_done ?? 0)} · ${ht(s.elapsed_ms ?? 0)}`;
  } else if (running) {
    pctEl.textContent = applying ? `${Math.round(pct)}%` : '…';
    pctEl.className = paused ? 'paused' : '';
    phaseEl.textContent = (paused ? '⏸ Paused — ' : '') + (phase ? PHASE_LABEL[phase] : '');
  }
  $('ro-items-done').textContent = `${dones.items} / ${totals.items}`;
  $('ro-items-left').textContent = `${Math.max(0, totals.items - dones.items)}`;
  $('ro-bytes-done').textContent = `${hb(dones.bytes)} / ${hb(totals.bytes)}`;
  $('ro-bytes-left').textContent = hb(Math.max(0, totals.bytes - dones.bytes));
  $('ro-elapsed').textContent = summary ? ht(summary.elapsed_ms ?? 0) : ht(activeNow());
  const r = windowRate(60000);
  if (summary) {
    $('ro-eta').textContent = '—';
  } else if (r && totals.bytes > 0 && r.bps > 1) {
    $('ro-eta').textContent = ht(Math.max(0, totals.bytes - dones.bytes) / r.bps * 1000) + ' remaining';
  } else if (r && totals.items > 0 && r.ips > 0.01) {
    $('ro-eta').textContent = ht(Math.max(0, totals.items - dones.items) / r.ips * 1000) + ' remaining';
  } else {
    $('ro-eta').textContent = 'Estimating…';
  }
  const r4 = windowRate(4000);
  rateBytesEl.textContent = r4 ? `${(r4.bps / (1 << 20)).toFixed(2)} MiB/s` : '';
  rateItemsEl.textContent = r4 ? `${r4.ips.toFixed(0)} items/s` : '';
  curfileEl.textContent = currentPath ? `‎${currentPath}` : '';
  curfileEl.title = currentPath;

  // window title + taskbar (fails silently: macOS has no taskbar progress)
  const title = summary
    ? (summary.cancelled ? 'Stopped — SyncDash' : `Done — SyncDash`)
    : applying ? `${Math.round(pct)}% — SyncDash` : `${phase ? PHASE_LABEL[phase] : 'Running'} — SyncDash`;
  win.setTitle(title).catch(() => {});
  if (summary) {
    win.setProgressBar({ status: ProgressBarStatus.None }).catch(() => {});
  } else if (applying) {
    win.setProgressBar({
      status: paused ? ProgressBarStatus.Paused : ProgressBarStatus.Normal,
      progress: Math.round(pct),
    }).catch(() => {});
  }
}

function renderErrors() {
  const ne = errors.filter((e) => !e.warning).length;
  const nw = errors.length - ne;
  errSec.classList.toggle('show', errors.length > 0);
  cntErr.textContent = ne ? `${ne} errors` : '';
  cntWarn.textContent = nw ? `${nw} warnings` : '';
  errList.innerHTML = '';
  for (const e of errors) {
    const d = document.createElement('div');
    d.className = 'erow' + (e.warning ? ' warn' : '');
    // narrative entries (kind=log) have no path/side — don't render an empty slot like "[scope/] msg"
    const tag = e.side ? `[${e.action}/${e.side}] ` : `[${e.action}] `;
    d.innerHTML = `<span class="epath mono">${escapeHtml(e.path)}</span> <span class="emsg">${tag}${escapeHtml(e.message)}</span>`;
    errList.appendChild(d);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!));
}

// When finished countdown

let cdTimer: number | null = null;

function countdownStop() {
  if (cdTimer !== null) { clearInterval(cdTimer); cdTimer = null; }
  cdEl.classList.remove('show');
}

function countdownStart(kind: string) {
  let left = 10;
  cdEl.classList.add('show');
  cdText.textContent = `${kind === 'sleep' ? 'Sleep' : 'Shut down'} in ${left}s`;
  cdFill.style.width = '100%';
  cdTimer = window.setInterval(() => {
    left -= 1;
    cdText.textContent = `${kind === 'sleep' ? 'Sleep' : 'Shut down'} in ${left}s`;
    cdFill.style.width = `${left * 10}%`;
    if (left <= 0) {
      countdownStop();
      invoke('post_sync_action', { kind }).catch(() => {});
    }
  }, 1000);
}

$('cd-cancel').addEventListener('click', countdownStop);

// Event handling

function onEvent(ev: RunEv) {
  if (ev.purpose === 'compare') return;     // compare belongs to the main-window panel; this window only shows apply
  if (ev.run_id < runId) return;            // late event from an already-cancelled run
  if (ev.run_id > runId) resetRun(ev.run_id, ev.ts_ms);

  switch (ev.kind) {
    case 'phase_start': {
      const p = ev.phase!;
      // tick off the previous phase's row
      if (phase && phase !== p) {
        const prev = stageRows.get(phase);
        if (prev) { prev.row.classList.remove('active'); prev.row.classList.add('done'); prev.ico.textContent = '✓'; }
      }
      phase = p;
      const r = stageRow(p);
      r.row.classList.add('active');
      if (ev.label) r.detail.textContent = ev.label;
      if (p === 'apply' || p === 'pack' || p === 'ship') {
        applying = true;
        pwin.classList.add('applying');
      }
      if (p === 'apply') {
        totals = { items: ev.items_total ?? 0, bytes: ev.bytes_total ?? 0 };
        dones = { items: 0, bytes: 0 };
        samples = [{ t: activeNow(), b: 0, i: 0 }];
      }
      break;
    }
    case 'totals':
      if (ev.phase === phase) totals = { items: ev.items_total ?? 0, bytes: ev.bytes_total ?? 0 };
      break;
    case 'progress': {
      const r = stageRow(ev.phase!);
      const it = ev.items_total ?? 0, bt = ev.bytes_total ?? 0;
      r.detail.textContent =
        `${ev.items_done ?? 0}${it ? ` / ${it}` : ''} items · ${hb(ev.bytes_done ?? 0)}${bt ? ` / ${hb(bt)}` : ''}`;
      if (ev.phase === 'apply') {
        dones = { items: ev.items_done ?? 0, bytes: ev.bytes_done ?? 0 };
        totals = { items: ev.items_total ?? totals.items, bytes: ev.bytes_total ?? totals.bytes };
        currentPath = ev.current_path ?? currentPath;
        samples.push({ t: activeNow(), b: dones.bytes, i: dones.items });
        if (samples.length > 4000) samples.splice(0, 1000);
      }
      break;
    }
    case 'error':
      errors.push({
        path: ev.path ?? '', action: ev.action ?? '', side: ev.side ?? '',
        message: ev.message ?? '', warning: ev.action === 'warning',
      });
      renderErrors();
      break;
    // v0.10: pipeline narrative. info stays out of this panel (it would drown the real problems),
    // only warn/error get in — they used to go to stderr only, which in a windowed build means never said at all.
    case 'log':
      if (ev.level === 'warn' || ev.level === 'error') {
        errors.push({
          path: '', action: ev.scope ?? 'log', side: '',
          message: ev.message ?? '', warning: ev.level === 'warn',
        });
        renderErrors();
      }
      break;
    case 'paused':
      pausedSince = Date.now();
      btnPause.textContent = '▶ Continue';
      break;
    case 'resumed':
      pausedSince = 0;
      pausedMs = ev.paused_ms ?? pausedMs;
      btnPause.textContent = '⏸ Pause';
      break;
    case 'summary': {
      summary = ev;
      running = false;
      pausedSince = 0;
      pausedMs = ev.paused_ms ?? pausedMs;
      // push dones to completion in the final state (throttling can swallow the last Progress event)
      if (!ev.cancelled && (ev.errors ?? 0) === 0 && totals.bytes + totals.items > 0) {
        dones = { items: totals.items, bytes: totals.bytes };
        samples.push({ t: activeNow(), b: dones.bytes, i: dones.items });
      }
      const cur = phase && stageRows.get(phase);
      if (cur) { cur.row.classList.remove('active'); cur.row.classList.add('done'); cur.ico.textContent = ev.cancelled ? '■' : '✓'; }
      btnPause.disabled = true;
      btnStop.disabled = true;
      renderReadouts();
      if (closeAfterStop) { win.close().catch(() => {}); break; }
      if (ev.cancelled) break;
      if ((ev.errors ?? 0) === 0 && chkAutoclose.checked && applying) {
        setTimeout(() => { if (summary) win.close().catch(() => {}); }, 1200);
        break;
      }
      const kind = selWfin.value;
      if (kind !== 'none' && applying) countdownStart(kind);
      break;
    }
  }
}

// Controls

btnPause.addEventListener('click', async () => {
  const wantPause = pausedSince === 0;
  btnPause.textContent = wantPause ? '▶ Continue' : '⏸ Pause';   // optimistic toggle, corrected once the event arrives
  if (wantPause) pausedSince = Date.now(); else { pausedSince = 0; }
  await invoke('pause_run', { paused: wantPause }).catch(() => {});
});

btnStop.addEventListener('click', async () => {
  btnStop.textContent = 'Stopping…';
  btnStop.disabled = true;
  // Stop pressed while paused: resume first, otherwise the cancel never reaches a checkpoint
  if (pausedSince) { pausedSince = 0; await invoke('pause_run', { paused: false }).catch(() => {}); }
  const had = await invoke<boolean>('cancel_run').catch(() => false);
  if (!had) {
    // no active run (it already finished on its own) — don't leave the button stuck on "Stopping…"
    btnStop.textContent = 'Finished';
  }
});

$('errhead').addEventListener('click', () => errSec.classList.toggle('open'));

chkAutoclose.checked = localStorage.getItem('sd.autoclose') === '1';
chkAutoclose.addEventListener('change', () => localStorage.setItem('sd.autoclose', chkAutoclose.checked ? '1' : '0'));
selWfin.value = localStorage.getItem('sd.whenfin') ?? 'none';
selWfin.addEventListener('change', () => localStorage.setItem('sd.whenfin', selWfin.value));

// Close button = FFS semantics: while running, cooperatively cancel first and leave once Summary arrives
win.onCloseRequested(async (e) => {
  if (running && !summary) {
    e.preventDefault();
    closeAfterStop = true;
    btnStop.textContent = 'Stopping…';
    btnStop.disabled = true;
    if (pausedSince) { pausedSince = 0; await invoke('pause_run', { paused: false }).catch(() => {}); }
    const had = await invoke<boolean>('cancel_run').catch(() => false);
    if (!had) {
      // nothing running on the engine side (Summary lost / already finished) — let the close through; never leave the window unclosable
      running = false;
      win.destroy().catch(() => {});
    }
  }
});

// Init

(async function init() {
  if (navigator.userAgent.includes('Macintosh')) document.body.classList.add('mac');
  await listen<RunEv>('run-progress', (e) => onEvent(e.payload));
  // 100ms redraw tick; the numeric readouts step every 500ms (same divider as FFS)
  setInterval(() => {
    if (runId < 0) return;
    if (applying) {
      drawGraph($('cv-bytes') as HTMLCanvasElement, 'b', totals.bytes);
      drawGraph($('cv-items') as HTMLCanvasElement, 'i', totals.items);
    }
    const now = Date.now();
    if (now - lastReadout >= 500 || summary) {
      lastReadout = now;
      renderReadouts();
    }
  }, 100);
})();
