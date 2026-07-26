// SyncDash 前端 v2：任务列表 → Compare（进度事件）→ 差异表（勾选/翻向/筛选/搜索）→ 确认 → Synchronize
// 反向 op 由 Rust 侧 reverse_op 预计算（reversed[i]），前端零同步语义，杜绝逻辑漂移。

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';

interface JobDto {
  name: string; mode: string; rigor: string; source: string; target: string; has_archive: boolean;
  remote: boolean; remote_host?: string; versioning: boolean; delta: boolean; parallel?: number;
  include: string[]; exclude: string[];
}
interface OpDto {
  side: 'source' | 'target';
  action: 'copy' | 'update' | 'move' | 'delete' | 'delete_dir' | 'chmod' | 'conflict' | 'note';
  path: string;
  from?: string;
  size?: number;
  mtime_ms?: number;
  hash?: string;
  mode?: number;
  reason: string;
}
interface PlanDto {
  header: { mode: string; source_root: string; target_root: string; op_count: number; conflict_count: number; source_entries: number; target_entries: number };
  ops: OpDto[];
  reversed: (OpDto | null)[];
}
interface ApplyDto { done: number; skipped: number; errors: number; bytes_copied: number; cancelled: boolean }
interface Progress { phase: string; detail: string; pct: number; rate: number }
interface PreflightDto { ok: boolean; blockers: string[]; warnings: string[] }

type Chip = 'all' | 'copy' | 'update' | 'move' | 'delete' | 'conflict';
const CHIPS: [Chip, string][] = [
  ['all', '全部'], ['copy', '复制'], ['update', '更新'], ['move', '移动'], ['delete', '删除'], ['conflict', '冲突/注'],
];

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const jobListEl = $('joblist');
const btnCompare = $<HTMLButtonElement>('btn-compare');
const btnSync = $<HTMLButtonElement>('btn-sync');
const chkHead = $<HTMLInputElement>('chk-head');
const statsEl = $('stats');
const spinEl = $('spin');
const pathEl = $('pathline');
const filterBar = $('filterbar');
const searchEl = $<HTMLInputElement>('search');
const chipsEl = $('chips');
const tableEl = $('plantable');
const bodyEl = $('planbody');
const emptyEl = $('empty');
const statusEl = $('status');
const modalEl = $('modal');
const modalBody = $('modal-body');
const modalOk = $('modal-ok') as HTMLButtonElement;

let jobs: JobDto[] = [];
let currentJob: JobDto | null = null;
let plan: PlanDto | null = null;
let checked: boolean[] = [];
let flipped: boolean[] = [];
let chip: Chip = 'all';
let search = '';
let busy = false;
/// M3 Overview：按顶层目录过滤（null = 不过滤；'(root)' = 根下散文件；'a' 或 'a/b' = 前缀）
let ovFilter: string | null = null;
let ovExpanded = new Set<string>();
/// 用户在确认单里勾了"我确认无误"（等同 CLI --i-know）；每次重新比对后归零
let acknowledged = false;

// ---------- 小工具 ----------

function setStatus(msg: string, cls: '' | 'err' | 'ok' = '') {
  statusEl.textContent = msg;
  statusEl.className = cls;
}

function setBusy(b: boolean) {
  busy = b;
  spinEl.classList.toggle('hidden', !b);
  btnCompare.disabled = b || !currentJob;
  btnSync.disabled = b || !plan || plan.ops.length === 0;
}

function humanSize(b?: number): string {
  if (b === undefined) return '';
  if (b >= 1 << 30) return (b / (1 << 30)).toFixed(2) + ' GB';
  if (b >= 1 << 20) return (b / (1 << 20)).toFixed(1) + ' MB';
  if (b >= 1024) return (b / 1024).toFixed(1) + ' KB';
  return b + ' B';
}

/// 该行当前生效的 op（翻向后取 reversed）
function eff(i: number): OpDto {
  const p = plan!;
  return flipped[i] && p.reversed[i] ? p.reversed[i]! : p.ops[i];
}

function selectable(op: OpDto): boolean {
  return op.action !== 'conflict' && op.action !== 'note';
}

function category(op: OpDto): Chip {
  switch (op.action) {
    case 'copy': return 'copy';
    case 'update': case 'chmod': return 'update';
    case 'move': return 'move';
    case 'delete': case 'delete_dir': return 'delete';
    default: return 'conflict';
  }
}

function matchesSearch(op: OpDto): boolean {
  if (!search) return true;
  const q = search.toLowerCase();
  return op.path.toLowerCase().includes(q)
    || (op.from ?? '').toLowerCase().includes(q)
    || op.reason.toLowerCase().includes(q);
}

function matchesOv(op: OpDto): boolean {
  if (!ovFilter) return true;
  if (ovFilter === '(root)') return !op.path.includes('/');
  return op.path === ovFilter || op.path.startsWith(ovFilter + '/');
}

function visibleIdx(): number[] {
  if (!plan) return [];
  const out: number[] = [];
  plan.ops.forEach((_, i) => {
    const op = eff(i);
    if ((chip === 'all' || category(op) === chip) && matchesSearch(op) && matchesOv(op)) out.push(i);
  });
  return out;
}

function badge(op: OpDto, canFlip: boolean): [string, string] {
  const toTarget = op.side === 'target';
  let txt = '', cls = '';
  switch (op.action) {
    case 'copy':   txt = toTarget ? '→ copy' : '← copy'; cls = toTarget ? 'copy-r' : 'copy-l'; break;
    case 'update': txt = toTarget ? '→ update' : '← update'; cls = 'update'; break;
    case 'move':   txt = (toTarget ? '→' : '←') + ' move'; cls = 'mv'; break;
    case 'delete':
    case 'delete_dir': txt = (toTarget ? '→' : '←') + ' delete'; cls = 'del'; break;
    case 'chmod':  txt = (toTarget ? '→' : '←') + ' chmod'; cls = 'update'; break;
    case 'conflict': txt = '⚡ conflict'; cls = 'conflict'; break;
    case 'note':   txt = 'ⓘ note'; cls = 'note'; break;
  }
  return [txt, cls + (canFlip ? ' flippable' : '')];
}

// ---------- 渲染 ----------

function renderChips() {
  chipsEl.innerHTML = '';
  if (!plan) return;
  const counts = new Map<Chip, number>();
  plan.ops.forEach((_, i) => {
    const c = category(eff(i));
    counts.set(c, (counts.get(c) ?? 0) + 1);
  });
  counts.set('all', plan.ops.length);
  for (const [key, label] of CHIPS) {
    const n = counts.get(key) ?? 0;
    const b = document.createElement('button');
    b.className = 'chip' + (chip === key ? ' on' : '') + (n === 0 ? ' zero' : '');
    b.textContent = `${label} ${n}`;
    b.addEventListener('click', () => { chip = key; renderAll(); });
    chipsEl.appendChild(b);
  }
}

/// M3：图标化统计条（FFS 底部统计条同款语义：0 值置灰、非 0 加粗）
function renderStats() {
  if (!plan) { statsEl.textContent = ''; return; }
  const cnt = { copy: 0, upd: 0, mv: 0, del: 0 };
  let bytes = 0;
  plan.ops.forEach((_, i) => {
    if (!checked[i]) return;
    const op = eff(i);
    switch (op.action) {
      case 'copy': cnt.copy++; bytes += op.size ?? 0; break;
      case 'update': case 'chmod': cnt.upd++; bytes += op.size ?? 0; break;
      case 'move': cnt.mv++; break;
      case 'delete': case 'delete_dir': cnt.del++; break;
    }
  });
  const flips = flipped.filter(Boolean).length;
  const seg = (cls: string, icon: string, n: number, title: string) =>
    `<span class="st ${cls}${n === 0 ? ' zero' : ''}" title="${title}">${icon}<b>${n}</b></span>`;
  statsEl.innerHTML =
    seg('s-copy', '＋', cnt.copy, '复制') +
    seg('s-upd', '✎', cnt.upd, '更新') +
    seg('s-mv', '⇢', cnt.mv, '移动（零重传）') +
    seg('s-del', '✕', cnt.del, '删除（进回收目录）') +
    seg('s-conf', '⚡', plan.header.conflict_count, '冲突') +
    `<span class="st${bytes === 0 ? ' zero' : ''}" title="待传字节">Σ<b>${humanSize(bytes) || '0 B'}</b></span>` +
    (flips ? `<span class="st" title="翻转方向">⇄<b>${flips}</b></span>` : '');
}

/// M3：Overview——按顶层目录聚合（条数/字节/占比条），点击过滤差异表，chevron 惰性展开二层
function renderOverview() {
  const listEl = $('ov-list');
  listEl.innerHTML = '';
  if (!plan || plan.ops.length === 0) return;
  interface Agg { items: number; bytes: number; children: Map<string, { items: number; bytes: number }> }
  const groups = new Map<string, Agg>();
  let totBytes = 0, totItems = 0;
  plan.ops.forEach((_, i) => {
    const op = eff(i);
    const slash = op.path.indexOf('/');
    const seg = slash < 0 ? '(root)' : op.path.slice(0, slash);
    let g = groups.get(seg);
    if (!g) { g = { items: 0, bytes: 0, children: new Map() }; groups.set(seg, g); }
    g.items++;
    g.bytes += op.size ?? 0;
    totItems++;
    totBytes += op.size ?? 0;
    if (slash >= 0) {
      const rest = op.path.slice(slash + 1);
      const slash2 = rest.indexOf('/');
      const seg2 = slash2 < 0 ? '(files)' : rest.slice(0, slash2);
      const c = g.children.get(seg2) ?? { items: 0, bytes: 0 };
      c.items++;
      c.bytes += op.size ?? 0;
      g.children.set(seg2, c);
    }
  });
  const share = (b: number, n: number) => (totBytes > 0 ? b / totBytes : totItems > 0 ? n / totItems : 0);
  const mkRow = (key: string, label: string, items: number, bytes: number, depth: number, hasKids: boolean) => {
    const row = document.createElement('div');
    row.className = 'ovrow' + (ovFilter === key ? ' on' : '') + (depth ? ' ovchild' : '');
    const pct = Math.round(share(bytes, items) * 100);
    row.innerHTML = `<div class="l1">` +
      (hasKids ? `<span class="chev">${ovExpanded.has(key) ? '▾' : '▸'}</span>` : `<span class="chev"></span>`) +
      `<span class="nm" title="${escapeHtml(label)}">${escapeHtml(label)}</span>` +
      `<span class="ct">${items} · ${humanSize(bytes) || '0 B'}</span></div>` +
      `<div class="ovbar"><div style="width:${pct}%"></div></div>`;
    row.addEventListener('click', (e) => {
      const onChev = (e.target as HTMLElement).classList.contains('chev');
      if (onChev && hasKids) {
        if (ovExpanded.has(key)) ovExpanded.delete(key); else ovExpanded.add(key);
      } else {
        ovFilter = ovFilter === key ? null : key;
      }
      renderAll();
    });
    return row;
  };
  const sorted = [...groups.entries()].sort((a, b) => b[1].bytes - a[1].bytes || b[1].items - a[1].items);
  for (const [seg, g] of sorted) {
    const hasKids = seg !== '(root)' && g.children.size > 0;
    listEl.appendChild(mkRow(seg, seg, g.items, g.bytes, 0, hasKids));
    if (hasKids && ovExpanded.has(seg)) {
      const kids = [...g.children.entries()].sort((a, b) => b[1].bytes - a[1].bytes || b[1].items - a[1].items);
      for (const [seg2, c] of kids) {
        if (seg2 === '(files)') continue;
        listEl.appendChild(mkRow(`${seg}/${seg2}`, seg2, c.items, c.bytes, 1, false));
      }
    }
  }
}

function renderTable() {
  bodyEl.innerHTML = '';
  const has = !!plan && plan.ops.length > 0;
  filterBar.classList.toggle('hidden', !has);
  tableEl.classList.toggle('hidden', !has);
  emptyEl.classList.toggle('hidden', has);
  if (!plan) { emptyEl.textContent = '← 选择任务，然后 Compare（Ctrl+R）'; return; }
  if (plan.ops.length === 0) { emptyEl.textContent = '✓ 两侧一致，没有需要同步的内容'; return; }

  const vis = visibleIdx();
  for (const i of vis) {
    const op = eff(i);
    const canFlip = !!plan.reversed[i] && selectable(plan.ops[i]);
    const tr = document.createElement('tr');
    if (!checked[i]) tr.classList.add('off');
    if (flipped[i]) tr.classList.add('flip');

    const tdChk = document.createElement('td');
    tdChk.className = 'c-chk';
    const cb = document.createElement('input');
    cb.type = 'checkbox';
    cb.checked = checked[i];
    cb.disabled = !selectable(op);
    cb.addEventListener('change', () => {
      checked[i] = cb.checked;
      tr.classList.toggle('off', !cb.checked);
      renderStats();
      syncHeadCheckbox();
    });
    tdChk.appendChild(cb);

    const tdAct = document.createElement('td');
    tdAct.className = 'c-act';
    const [txt, cls] = badge(op, canFlip);
    const span = document.createElement('span');
    span.className = 'badge ' + cls;
    span.textContent = txt;
    if (canFlip) {
      span.title = '点按翻转方向（再点恢复）';
      span.addEventListener('click', () => {
        flipped[i] = !flipped[i];
        renderAll();
      });
    }
    tdAct.appendChild(span);

    const tdPath = document.createElement('td');
    tdPath.className = 'mono';
    tdPath.textContent = op.path;
    tdPath.title = op.path;

    const tdFrom = document.createElement('td');
    tdFrom.className = 'mono dim';
    tdFrom.textContent = op.from ?? '';

    const tdSize = document.createElement('td');
    tdSize.className = 'c-size mono';
    tdSize.textContent = humanSize(op.size);

    const tdReason = document.createElement('td');
    tdReason.className = 'reason';
    tdReason.textContent = op.reason;

    tr.append(tdChk, tdAct, tdPath, tdFrom, tdSize, tdReason);
    bodyEl.appendChild(tr);
  }
  syncHeadCheckbox();
  renderStats();
}

function syncHeadCheckbox() {
  const vis = visibleIdx().filter((i) => selectable(eff(i)));
  chkHead.checked = vis.length > 0 && vis.every((i) => checked[i]);
}

function renderAll() {
  renderChips();
  renderOverview();
  renderTable();
}

function renderJobs() {
  jobListEl.innerHTML = '';
  for (const j of jobs) {
    const div = document.createElement('div');
    div.className = 'job' + (currentJob?.name === j.name ? ' active' : '');
    const rigor = j.rigor && j.rigor !== 'standard' ? `<span class="rigor">${j.rigor}</span>` : '';
    const remote = j.remote ? `<span class="rbadge">ssh</span>` : '';
    div.innerHTML = `<span class="name">${j.name}</span>${remote}${rigor}<span class="mode ${j.mode}">${j.mode}</span>`;
    div.title = `${j.source}\n→ ${j.target}` + (j.remote ? `\nssh:${j.remote_host ?? ''}` : '');
    div.addEventListener('click', () => {
      if (busy) return;
      currentJob = j;
      plan = null; checked = []; flipped = []; chip = 'all'; search = ''; searchEl.value = '';
      ovFilter = null; ovExpanded.clear();
      renderJobs();
      renderAll();
      pathEl.textContent = `${j.source}   ⇄   ${j.target}`;
      btnCompare.disabled = false;
      btnSync.disabled = true;
      setStatus(`已选 '${j.name}'（${j.mode}${j.has_archive ? '，带 archive' : ''}${j.rigor !== 'standard' ? '，' + j.rigor : ''}）— Compare 开始比对`);
    });
    jobListEl.appendChild(div);
  }
}

// ---------- 动作 ----------

async function doCompare(showWindow = true) {
  if (!currentJob || busy) return;
  setBusy(true);
  setStatus(`正在比对 '${currentJob.name}' ...`);
  // FFS 同款：比对期也用进度子窗（扫描双侧的条数/字节实时跳动），结束后自动收起
  if (showWindow) invoke('open_progress_window').catch(() => {});
  try {
    acknowledged = false;
    modalOk.disabled = false;
    plan = await invoke<PlanDto>('compare_job', { name: currentJob.name });
    checked = plan.ops.map((op) => selectable(op));
    flipped = plan.ops.map(() => false);
    chip = 'all';
    ovFilter = null; ovExpanded.clear();
    renderAll();
    setStatus(
      plan.ops.length === 0
        ? `'${currentJob.name}' 两侧一致 ✓`
        : `'${currentJob.name}'：${plan.ops.length} 项，冲突 ${plan.header.conflict_count} — 审阅后 Synchronize（Enter）`,
      plan.header.conflict_count > 0 ? 'err' : '',
    );
  } catch (e) {
    plan = null;
    renderAll();
    setStatus(String(e) === 'cancelled' ? '比对已取消' : `比对失败：${e}`, String(e) === 'cancelled' ? '' : 'err');
  }
  if (showWindow) invoke('close_progress_window').catch(() => {});
  setBusy(false);
}

async function openConfirm() {
  if (!currentJob || !plan || busy) return;
  const idx = checked.map((c, i) => (c ? i : -1)).filter((i) => i >= 0);
  if (idx.length === 0) { setStatus('没有勾选任何项', 'err'); return; }
  const cnt = { copy: 0, update: 0, move: 0, del: 0 };
  let bytes = 0, delBytes = 0;
  for (const i of idx) {
    const op = eff(i);
    if (op.action === 'copy') { cnt.copy++; bytes += op.size ?? 0; }
    else if (op.action === 'update') { cnt.update++; bytes += op.size ?? 0; }
    else if (op.action === 'move') cnt.move++;
    else if (op.action === 'delete' || op.action === 'delete_dir') { cnt.del++; delBytes += op.size ?? 0; }
  }
  modalBody.innerHTML = `
    <div class="mrow"><span>任务</span><b>${currentJob.name}</b><span class="mode ${currentJob.mode}">${currentJob.mode}</span></div>
    <div class="mrow"><span>复制 / 更新</span><b>${cnt.copy} / ${cnt.update}</b><span class="dim">${humanSize(bytes) || '0 B'}</span></div>
    <div class="mrow"><span>移动（零重传）</span><b>${cnt.move}</b></div>
    <div class="mrow ${cnt.del ? 'danger' : ''}"><span>删除（进回收目录）</span><b>${cnt.del}</b><span class="dim">${cnt.del ? humanSize(delBytes) : ''}</span></div>
    ${flipped.some(Boolean) ? `<div class="mrow warn"><span>其中翻转方向</span><b>${flipped.filter(Boolean).length}</b></div>` : ''}
  `;
  modalEl.classList.remove('hidden');

  // 闸门体检（磁盘空间 / 删除占比）——理由要摆在按下 Synchronize 之前，
  // 而不是等执行时才在看不见的 stderr 里出现
  try {
    const pf = await invoke<PreflightDto>('preflight', {
      name: currentJob.name, plan, ops: idx.map((i) => eff(i)), acknowledged: acknowledged,
    });
    for (const w of pf.warnings) {
      modalBody.innerHTML += `<div class="mrow warn"><span>提醒</span><span class="dim">${escapeHtml(w)}</span></div>`;
    }
    if (!pf.ok) {
      for (const b of pf.blockers) {
        modalBody.innerHTML += `<div class="mrow danger"><span>拒绝执行</span><span class="dim">${escapeHtml(b)}</span></div>`;
      }
      modalBody.innerHTML += `<div class="mrow"><label><input type="checkbox" id="ackbox"> 我确认无误，继续（等同 CLI 的 --i-know）</label></div>`;
      const box = document.getElementById('ackbox') as HTMLInputElement | null;
      if (box) box.onchange = () => { acknowledged = box.checked; };
      modalOk.disabled = true;
      if (box) box.addEventListener('change', () => { modalOk.disabled = !box.checked; });
    } else {
      modalOk.disabled = false;
    }
  } catch (e) {
    modalBody.innerHTML += `<div class="mrow danger"><span>体检失败</span><span class="dim">${escapeHtml(String(e))}</span></div>`;
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!));
}

async function doSync() {
  modalEl.classList.add('hidden');
  if (!currentJob || !plan || busy) return;
  const idx = checked.map((c, i) => (c ? i : -1)).filter((i) => i >= 0);
  const finalOps = idx.map((i) => eff(i));
  setBusy(true);
  setStatus(`正在同步 '${currentJob.name}'（${finalOps.length} 项）...`);
  // 同步期的子窗去留归它自己的 Auto-close / When-finished，主窗不收
  invoke('open_progress_window').catch(() => {});
  try {
    const r = await invoke<ApplyDto>('apply_job', { name: currentJob.name, plan, ops: finalOps, acknowledged });
    setStatus(
      r.cancelled
        ? `已停止：${r.done} 执行后取消 — 复核中...`
        : `完成：${r.done} 执行，${r.skipped} 跳过，${r.errors} 错误 — 复核中...`,
      r.errors ? 'err' : 'ok',
    );
    setBusy(false);
    await doCompare(false);
  } catch (e) {
    setStatus(`同步失败：${e}`, 'err');
    setBusy(false);
  }
}

// ---------- 事件接线 ----------

btnCompare.addEventListener('click', () => doCompare());
btnSync.addEventListener('click', openConfirm);
$('modal-ok').addEventListener('click', doSync);
$('modal-cancel').addEventListener('click', () => modalEl.classList.add('hidden'));
modalEl.addEventListener('click', (e) => { if (e.target === modalEl) modalEl.classList.add('hidden'); });

chkHead.addEventListener('change', () => {
  const vis = visibleIdx();
  for (const i of vis) if (selectable(eff(i))) checked[i] = chkHead.checked;
  renderTable();
});

searchEl.addEventListener('input', () => {
  search = searchEl.value.trim();
  renderTable();
});

// M3 Overview 折叠/清除
const ovEl = $('overview');
ovEl.classList.toggle('collapsed', localStorage.getItem('sd.ov') !== 'open');
$('ov-toggle').addEventListener('click', () => {
  ovEl.classList.toggle('collapsed');
  localStorage.setItem('sd.ov', ovEl.classList.contains('collapsed') ? 'closed' : 'open');
});
$('ov-clear').addEventListener('click', () => { ovFilter = null; renderAll(); });

document.addEventListener('keydown', (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (!modalEl.classList.contains('hidden')) {
    if (e.key === 'Escape') modalEl.classList.add('hidden');
    if (e.key === 'Enter' && !modalOk.disabled) doSync();
    return;
  }
  if (mod && e.key.toLowerCase() === 'r') { e.preventDefault(); doCompare(); }
  else if (mod && e.key.toLowerCase() === 'f') { e.preventDefault(); searchEl.focus(); }
  else if (e.key === 'Enter' && document.activeElement !== searchEl && plan && !busy && !btnSync.disabled) openConfirm();
});

// ---------- 初始化 ----------

(async function init() {
  if (navigator.userAgent.includes('Macintosh')) document.body.classList.add('mac');
  await listen<Progress>('progress', (ev) => {
    const { phase, detail, pct, rate } = ev.payload;
    const map: Record<string, string> = {
      'scan-source': '扫描 source：', 'scan-target': '扫描 target：', 'comparing': '比对中：', 'warning': '⚠ ',
    };
    const suffix = pct >= 0 ? `  ${pct}%${rate > 0 ? `  ${rate.toFixed(1)} MiB/s` : ''}` : '';
    setStatus((map[phase] ?? phase) + detail + suffix, phase === 'warning' ? 'err' : '');
  });
  try {
    jobs = await invoke<JobDto[]>('list_jobs');
    renderJobs();
    $('jobsdir').textContent = await invoke<string>('jobs_dir');
    try { $('appver').textContent = 'v' + (await getVersion()); } catch { /* 权限缺省时忽略 */ }
    setStatus(jobs.length ? '选择左侧任务开始' : '没有任务 — 在 jobs 目录放 <名字>.toml');
  } catch (e) {
    setStatus(`初始化失败：${e}`, 'err');
  }
})();
