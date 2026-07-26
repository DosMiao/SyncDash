// SyncDash 前端：任务列表 → Compare → 差异表（勾选）→ Synchronize
// 全部数据经 tauri invoke 走核心库；本文件只管渲染与交互。

import { invoke } from '@tauri-apps/api/core';

interface JobDto { name: string; mode: string; source: string; target: string; has_archive: boolean }
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
}
interface ApplyDto { done: number; skipped: number; errors: number }

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const jobListEl = $('joblist');
const btnCompare = $<HTMLButtonElement>('btn-compare');
const btnSync = $<HTMLButtonElement>('btn-sync');
const chkAll = $<HTMLInputElement>('chk-all');
const statsEl = $('stats');
const spinEl = $('spin');
const pathEl = $('pathline');
const tableEl = $('plantable');
const bodyEl = $('planbody');
const emptyEl = $('empty');
const statusEl = $('status');

let jobs: JobDto[] = [];
let currentJob: JobDto | null = null;
let plan: PlanDto | null = null;
let checked: boolean[] = [];
let busy = false;

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

function selectable(op: OpDto): boolean {
  return op.action !== 'conflict' && op.action !== 'note';
}

function badge(op: OpDto): [string, string] {
  const toTarget = op.side === 'target';
  switch (op.action) {
    case 'copy':   return [toTarget ? '→ copy' : '← copy', toTarget ? 'copy-r' : 'copy-l'];
    case 'update': return [toTarget ? '→ update' : '← update', 'update'];
    case 'move':   return ['⇢ move', 'mv'];
    case 'delete':
    case 'delete_dir': return ['✕ delete', 'del'];
    case 'conflict': return ['⚡ conflict', 'conflict'];
    case 'note':   return ['ⓘ note', 'note'];
  }
}

function renderStats() {
  if (!plan) { statsEl.textContent = ''; return; }
  const sel = checked.filter(Boolean).length;
  let bytes = 0;
  plan.ops.forEach((op, i) => {
    if (checked[i] && (op.action === 'copy' || op.action === 'update') && op.size) bytes += op.size;
  });
  statsEl.textContent =
    `${plan.ops.length} 项 | 已选 ${sel} | 待传 ${humanSize(bytes)} | 冲突 ${plan.header.conflict_count}`;
}

function renderTable() {
  bodyEl.innerHTML = '';
  const has = !!plan && plan.ops.length > 0;
  tableEl.classList.toggle('hidden', !has);
  emptyEl.classList.toggle('hidden', has);
  if (!plan) { emptyEl.textContent = '← 选择任务，然后 Compare'; return; }
  if (plan.ops.length === 0) { emptyEl.textContent = '✓ 两侧一致，没有需要同步的内容'; return; }

  plan.ops.forEach((op, i) => {
    const tr = document.createElement('tr');
    if (!checked[i]) tr.classList.add('off');

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
    });
    tdChk.appendChild(cb);

    const tdAct = document.createElement('td');
    tdAct.className = 'c-act';
    const [txt, cls] = badge(op);
    tdAct.innerHTML = `<span class="badge ${cls}">${txt}</span>`;

    const tdPath = document.createElement('td');
    tdPath.className = 'mono';
    tdPath.textContent = op.path;

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
  });
  renderStats();
}

function renderJobs() {
  jobListEl.innerHTML = '';
  for (const j of jobs) {
    const div = document.createElement('div');
    div.className = 'job' + (currentJob?.name === j.name ? ' active' : '');
    div.innerHTML = `<span class="name">${j.name}</span><span class="mode ${j.mode}">${j.mode}</span>`;
    div.title = `${j.source}\n→ ${j.target}`;
    div.addEventListener('click', () => {
      if (busy) return;
      currentJob = j;
      plan = null;
      checked = [];
      renderJobs();
      renderTable();
      pathEl.textContent = `${j.source}   →   ${j.target}`;
      btnCompare.disabled = false;
      btnSync.disabled = true;
      setStatus(`已选 '${j.name}'（${j.mode}${j.has_archive ? '，带 archive' : ''}）— 点 Compare`);
    });
    jobListEl.appendChild(div);
  }
}

async function doCompare() {
  if (!currentJob || busy) return;
  setBusy(true);
  setStatus(`正在比对 '${currentJob.name}'（扫描双侧、哈希变动文件）...`);
  try {
    plan = await invoke<PlanDto>('compare_job', { name: currentJob.name });
    checked = plan.ops.map(selectable);
    renderTable();
    setStatus(
      plan.ops.length === 0
        ? `'${currentJob.name}' 两侧一致 ✓`
        : `'${currentJob.name}'：${plan.ops.length} 项，冲突 ${plan.header.conflict_count} — 勾选后 Synchronize`,
      plan.header.conflict_count > 0 ? 'err' : '',
    );
  } catch (e) {
    plan = null;
    renderTable();
    setStatus(`比对失败：${e}`, 'err');
  }
  setBusy(false);
}

async function doSync() {
  if (!currentJob || !plan || busy) return;
  const selected = checked.map((c, i) => (c ? i : -1)).filter((i) => i >= 0);
  if (selected.length === 0) { setStatus('没有勾选任何项', 'err'); return; }
  setBusy(true);
  setStatus(`正在同步 '${currentJob.name}'（${selected.length} 项）...`);
  try {
    const r = await invoke<ApplyDto>('apply_job', { name: currentJob.name, plan, selected });
    setStatus(`完成：${r.done} 执行，${r.skipped} 跳过，${r.errors} 错误 — 复核中...`, r.errors ? 'err' : 'ok');
    await doCompare(); // 自动复比对验证收敛
  } catch (e) {
    setStatus(`同步失败：${e}`, 'err');
  }
  setBusy(false);
}

btnCompare.addEventListener('click', doCompare);
btnSync.addEventListener('click', doSync);
chkAll.addEventListener('change', () => {
  if (!plan) return;
  plan.ops.forEach((op, i) => { checked[i] = chkAll.checked && selectable(op); });
  renderTable();
});

(async function init() {
  try {
    jobs = await invoke<JobDto[]>('list_jobs');
    renderJobs();
    $('jobsdir').textContent = await invoke<string>('jobs_dir');
    setStatus(jobs.length ? '选择左侧任务开始' : '没有任务 — 在 jobs 目录放 <名字>.toml');
  } catch (e) {
    setStatus(`初始化失败：${e}`, 'err');
  }
})();
