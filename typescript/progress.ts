// SyncDash 进度子窗口（v0.9 M2，FFS 同款行为参数）：
// - 速率 = 4s 滑窗，ETA = 60s 滑窗，读数 500ms 刷新，重绘 100ms
// - 百分比 = (bytesDone + itemsDone) / (bytesTotal + itemsTotal)（FFS 同式）
// - 暂停不计入 elapsed；停止=协作取消（完成当前块后收手，终点绝无半截文件）
// - 错误累积不中断；Auto-close 与 When-finished（睡眠/关机，10s 可取消倒计时）
// 数据源：Rust TauriSink 广播的 `run-progress` 事件（≥100ms/条已节流，带 run_id）。

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow, ProgressBarStatus } from '@tauri-apps/api/window';

type PhaseName =
  | 'scan-source' | 'scan-target' | 'compare' | 'apply'
  | 'pack' | 'ship' | 'verify' | 'refresh';

interface RunEv {
  run_id: number;
  kind: 'phase_start' | 'totals' | 'progress' | 'error' | 'paused' | 'resumed' | 'summary';
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
  'scan-source': '扫描 source',
  'scan-target': '扫描 target',
  'compare': '比对',
  'apply': '同步',
  'pack': '打包',
  'ship': '传输',
  'verify': '校验',
  'refresh': '刷新存档',
};

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const win = getCurrentWindow();

// ---------- 运行状态 ----------

interface Sample { t: number; b: number; i: number } // t = 活动毫秒（去除暂停）

let runId = -1;
let running = false;
let t0 = 0;                       // 本次运行首事件 ts
let pausedMs = 0;                 // 引擎权威累计（Resumed/Summary 刷新）
let pausedSince = 0;              // 正在暂停时的起点（本地估算）
let phase: PhaseName | null = null;
let applying = false;            // 进入过 apply/pack/ship → 图表模式
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
  btnPause.textContent = '⏸ 暂停';
  btnStop.disabled = false;
  btnStop.textContent = '■ 停止';
  pctEl.className = '';
  renderReadouts();
}

// ---------- DOM ----------

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

// ---------- 速率 / ETA（滑窗对样本序列做差） ----------

function windowRate(windowMs: number): { bps: number; ips: number } | null {
  if (samples.length < 2) return null;
  const last = samples[samples.length - 1];
  const cut = last.t - windowMs;
  // 从尾部找第一个 <= cut 的样本；样本最老不足窗口时用最老的（FFS 同款“窗口未满用实际跨度”）
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

// ---------- 图表 ----------

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
  // ETA 终点参与 x 轴：完成估计落在图右侧（FFS 楔形的降级版：虚线标记）
  const r = windowRate(60000);
  const rate = key === 'b' ? r?.bps ?? 0 : r?.ips ?? 0;
  const remain = Math.max(0, total - lastV);
  const etaMs = rate > 1e-6 && total > 0 ? remain / rate * 1000 : 0;
  const xMax = Math.max(now + etaMs, now, 1000);
  const yMax = Math.max(total, lastV, 1);
  const X = (t: number) => t / xMax * (w - 8) + 4;
  const Y = (v: number) => h - 6 - v / yMax * (h - 16);

  // 网格
  ctx.strokeStyle = cBorder; ctx.lineWidth = 1;
  for (let g = 1; g <= 3; g++) {
    const y = 6 + (h - 12) * g / 4;
    ctx.beginPath(); ctx.moveTo(2, y); ctx.lineTo(w - 2, y); ctx.stroke();
  }
  // 总量虚线
  if (total > 0) {
    ctx.setLineDash([4, 3]); ctx.strokeStyle = cDim;
    ctx.beginPath(); ctx.moveTo(2, Y(total)); ctx.lineTo(w - 2, Y(total)); ctx.stroke();
    ctx.setLineDash([]);
  }
  // 累积曲线 + 填充（样本 >300 时步进抽稀）
  if (samples.length > 1) {
    const step = Math.max(1, Math.floor(samples.length / 300));
    ctx.beginPath(); ctx.moveTo(X(0), Y(0));
    for (let k = 0; k < samples.length; k += step) ctx.lineTo(X(samples[k].t), Y(samples[k][key]));
    ctx.lineTo(X(samples[samples.length - 1].t), Y(lastV));
    ctx.strokeStyle = cAccent; ctx.lineWidth = 1.5; ctx.stroke();
    ctx.lineTo(X(samples[samples.length - 1].t), Y(0)); ctx.closePath();
    ctx.fillStyle = cAccent + '33'; ctx.fill();
  }
  // now 竖线 + 预计完成虚线
  ctx.strokeStyle = cDim;
  ctx.beginPath(); ctx.moveTo(X(now), 4); ctx.lineTo(X(now), h - 4); ctx.stroke();
  if (etaMs > 0) {
    ctx.setLineDash([3, 3]);
    ctx.beginPath(); ctx.moveTo(X(now + etaMs), 4); ctx.lineTo(X(now + etaMs), h - 4); ctx.stroke();
    ctx.setLineDash([]);
  }
}

// ---------- 渲染 ----------

let lastReadout = 0;

function renderReadouts() {
  const pct = percent();
  const paused = pausedSince > 0;
  if (summary) {
    const s = summary;
    pctEl.textContent = s.cancelled ? '已停止' : `${Math.round(percent())}%`;
    pctEl.className = s.cancelled ? 'err' : (s.errors ? 'err' : 'ok');
    phaseEl.textContent = s.cancelled
      ? `已取消 — ${s.done} 执行，${s.skipped} 跳过`
      : `完成 — ${s.done} 执行，${s.skipped} 跳过，${s.errors} 错误 · ${hb(s.bytes_done ?? 0)} · ${ht(s.elapsed_ms ?? 0)}`;
  } else if (running) {
    pctEl.textContent = applying ? `${Math.round(pct)}%` : '…';
    pctEl.className = paused ? 'paused' : '';
    phaseEl.textContent = (paused ? '⏸ 已暂停 — ' : '') + (phase ? PHASE_LABEL[phase] : '');
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
    $('ro-eta').textContent = ht(Math.max(0, totals.bytes - dones.bytes) / r.bps * 1000) + ' 剩余';
  } else if (r && totals.items > 0 && r.ips > 0.01) {
    $('ro-eta').textContent = ht(Math.max(0, totals.items - dones.items) / r.ips * 1000) + ' 剩余';
  } else {
    $('ro-eta').textContent = '估算中…';
  }
  const r4 = windowRate(4000);
  rateBytesEl.textContent = r4 ? `${(r4.bps / (1 << 20)).toFixed(2)} MiB/s` : '';
  rateItemsEl.textContent = r4 ? `${r4.ips.toFixed(0)} 项/s` : '';
  curfileEl.textContent = currentPath ? `‎${currentPath}` : '';
  curfileEl.title = currentPath;

  // 窗题 + 任务栏（失败静默：mac 无任务栏进度）
  const title = summary
    ? (summary.cancelled ? '已停止 — SyncDash' : `完成 — SyncDash`)
    : applying ? `${Math.round(pct)}% — SyncDash` : `${phase ? PHASE_LABEL[phase] : '运行'} — SyncDash`;
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
  cntErr.textContent = ne ? `${ne} 个错误` : '';
  cntWarn.textContent = nw ? `${nw} 个提醒` : '';
  errList.innerHTML = '';
  for (const e of errors) {
    const d = document.createElement('div');
    d.className = 'erow' + (e.warning ? ' warn' : '');
    d.innerHTML = `<span class="epath mono">${escapeHtml(e.path || e.action)}</span> <span class="emsg">[${e.action}/${e.side}] ${escapeHtml(e.message)}</span>`;
    errList.appendChild(d);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!));
}

// ---------- When finished 倒计时 ----------

let cdTimer: number | null = null;

function countdownStop() {
  if (cdTimer !== null) { clearInterval(cdTimer); cdTimer = null; }
  cdEl.classList.remove('show');
}

function countdownStart(kind: string) {
  let left = 10;
  cdEl.classList.add('show');
  cdText.textContent = `${left} 秒后${kind === 'sleep' ? '睡眠' : '关机'}`;
  cdFill.style.width = '100%';
  cdTimer = window.setInterval(() => {
    left -= 1;
    cdText.textContent = `${left} 秒后${kind === 'sleep' ? '睡眠' : '关机'}`;
    cdFill.style.width = `${left * 10}%`;
    if (left <= 0) {
      countdownStop();
      invoke('post_sync_action', { kind }).catch(() => {});
    }
  }, 1000);
}

$('cd-cancel').addEventListener('click', countdownStop);

// ---------- 事件处理 ----------

function onEvent(ev: RunEv) {
  if (ev.run_id < runId) return;            // 已取消运行的迟到事件
  if (ev.run_id > runId) resetRun(ev.run_id, ev.ts_ms);

  switch (ev.kind) {
    case 'phase_start': {
      const p = ev.phase!;
      // 上一阶段的行画上勾
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
        `${ev.items_done ?? 0}${it ? ` / ${it}` : ''} 项 · ${hb(ev.bytes_done ?? 0)}${bt ? ` / ${hb(bt)}` : ''}`;
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
    case 'paused':
      pausedSince = Date.now();
      btnPause.textContent = '▶ 继续';
      break;
    case 'resumed':
      pausedSince = 0;
      pausedMs = ev.paused_ms ?? pausedMs;
      btnPause.textContent = '⏸ 暂停';
      break;
    case 'summary': {
      summary = ev;
      running = false;
      pausedSince = 0;
      pausedMs = ev.paused_ms ?? pausedMs;
      // 终态时把 dones 顶到位（丢帧节流可能吞掉最后一条 Progress）
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

// ---------- 控件 ----------

btnPause.addEventListener('click', async () => {
  const wantPause = pausedSince === 0;
  btnPause.textContent = wantPause ? '▶ 继续' : '⏸ 暂停';   // 乐观切换，事件到达后校准
  if (wantPause) pausedSince = Date.now(); else { pausedSince = 0; }
  await invoke('pause_run', { paused: wantPause }).catch(() => {});
});

btnStop.addEventListener('click', async () => {
  btnStop.textContent = '正在停止…';
  btnStop.disabled = true;
  // 卡在暂停里点停止：先放行，取消才能被检查点看见
  if (pausedSince) { pausedSince = 0; await invoke('pause_run', { paused: false }).catch(() => {}); }
  await invoke('cancel_run').catch(() => {});
});

$('errhead').addEventListener('click', () => errSec.classList.toggle('open'));

chkAutoclose.checked = localStorage.getItem('sd.autoclose') === '1';
chkAutoclose.addEventListener('change', () => localStorage.setItem('sd.autoclose', chkAutoclose.checked ? '1' : '0'));
selWfin.value = localStorage.getItem('sd.whenfin') ?? 'none';
selWfin.addEventListener('change', () => localStorage.setItem('sd.whenfin', selWfin.value));

// 关闭钮 = FFS 语义：运行中先协作取消，Summary 到了再走
win.onCloseRequested(async (e) => {
  if (running && !summary) {
    e.preventDefault();
    closeAfterStop = true;
    btnStop.textContent = '正在停止…';
    btnStop.disabled = true;
    if (pausedSince) { pausedSince = 0; await invoke('pause_run', { paused: false }).catch(() => {}); }
    await invoke('cancel_run').catch(() => {});
  }
});

// ---------- 初始化 ----------

(async function init() {
  if (navigator.userAgent.includes('Macintosh')) document.body.classList.add('mac');
  await listen<RunEv>('run-progress', (e) => onEvent(e.payload));
  // 100ms 重绘节拍；数字读数 500ms 一跳（FFS 同款分频）
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
