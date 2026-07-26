// SyncDash 前端 v2：任务列表 → Compare（进度事件）→ 差异表（勾选/翻向/筛选/搜索）→ 确认 → Synchronize
// 反向 op 由 Rust 侧 reverse_op 预计算（reversed[i]），前端零同步语义，杜绝逻辑漂移。

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';

interface JobDto { name: string; mode: string; rigor: string; source: string; target: string; has_archive: boolean }
interface OpDto {
  side: 'source' | 'target';
  action: 'copy' | 'update' | 'move' | 'delete' | 'delete_dir' | 'conflict' | 'note';
  path: string;
  from?: string;
  size?: number;
  mtime_ms?: number;
  hash?: string;
  reason: string;
}
interface PlanDto {
  header: { mode: string; source_root: string; target_root: string; op_count: number; conflict_count: number };
  ops: OpDto[];
  reversed: (OpDto | null)[];
}
interface ApplyDto { done: number; skipped: number; errors: number }
interface Progress { phase: string; detail: string }

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

let jobs: JobDto[] = [];
let currentJob: JobDto | null = null;
let plan: PlanDto | null = null;
let checked: boolean[] = [];
let flipped: boolean[] = [];
let chip: Chip = 'all';
let search = '';
let busy = false;

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
    case 'update': return 'update';
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

function visibleIdx(): number[] {
  if (!plan) return [];
  const out: number[] = [];
  plan.ops.forEach((_, i) => {
    const op = eff(i);
    if ((chip === 'all' || category(op) === chip) && matchesSearch(op)) out.push(i);
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

function renderStats() {
  if (!plan) { statsEl.textContent = ''; return; }
  const selN = checked.filter(Boolean).length;
  let bytes = 0;
  plan.ops.forEach((_, i) => {
    const op = eff(i);
    if (checked[i] && (op.action === 'copy' || op.action === 'update') && op.size) bytes += op.size;
  });
  const flips = flipped.filter(Boolean).length;
  statsEl.textContent =
    `${plan.ops.length} 项 · 已选 ${selN}${flips ? ` · 翻向 ${flips}` : ''} · 待传 ${humanSize(bytes) || '0 B'} · 冲突 ${plan.header.conflict_count}`;
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
  renderTable();
}

function renderJobs() {
  jobListEl.innerHTML = '';
  for (const j of jobs) {
    const div = document.createElement('div');
    div.className = 'job' + (currentJob?.name === j.name ? ' active' : '');
    const rigor = j.rigor && j.rigor !== 'standard' ? `<span class="rigor">${j.rigor}</span>` : '';
    div.innerHTML = `<span class="name">${j.name}</span>${rigor}<span class="mode ${j.mode}">${j.mode}</span>`;
    div.title = `${j.source}\n→ ${j.target}`;
    div.addEventListener('click', () => {
      if (busy) return;
      currentJob = j;
      plan = null; checked = []; flipped = []; chip = 'all'; search = ''; searchEl.value = '';
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

async function doCompare() {
  if (!currentJob || busy) return;
  setBusy(true);
  setStatus(`正在比对 '${currentJob.name}' ...`);
  try {
    plan = await invoke<PlanDto>('compare_job', { name: currentJob.name });
    checked = plan.ops.map((op) => selectable(op));
    flipped = plan.ops.map(() => false);
    chip = 'all';
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
    setStatus(`比对失败：${e}`, 'err');
  }
  setBusy(false);
}

function openConfirm() {
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
}

async function doSync() {
  modalEl.classList.add('hidden');
  if (!currentJob || !plan || busy) return;
  const idx = checked.map((c, i) => (c ? i : -1)).filter((i) => i >= 0);
  const finalOps = idx.map((i) => eff(i));
  setBusy(true);
  setStatus(`正在同步 '${currentJob.name}'（${finalOps.length} 项）...`);
  try {
    const r = await invoke<ApplyDto>('apply_job', { name: currentJob.name, plan, ops: finalOps });
    setStatus(`完成：${r.done} 执行，${r.skipped} 跳过，${r.errors} 错误 — 复核中...`, r.errors ? 'err' : 'ok');
    setBusy(false);
    await doCompare();
  } catch (e) {
    setStatus(`同步失败：${e}`, 'err');
    setBusy(false);
  }
}

// ---------- 事件接线 ----------

btnCompare.addEventListener('click', doCompare);
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

document.addEventListener('keydown', (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (!modalEl.classList.contains('hidden')) {
    if (e.key === 'Escape') modalEl.classList.add('hidden');
    if (e.key === 'Enter') doSync();
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
    const { phase, detail } = ev.payload;
    const map: Record<string, string> = {
      'scan-source': '扫描 source：', 'scan-target': '扫描 target：', 'comparing': '比对中：',
    };
    setStatus((map[phase] ?? phase) + detail);
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
