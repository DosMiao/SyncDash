// SyncDash 前端 v2：任务列表 → Compare（进度事件）→ 差异表（勾选/翻向/筛选/搜索）→ 确认 → Synchronize
// 反向 op 由 Rust 侧 reverse_op 预计算（reversed[i]），前端零同步语义，杜绝逻辑漂移。

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';
import { getCurrentWebview } from '@tauri-apps/api/webview';

interface JobDto {
  name: string; mode: string; rigor: string; source: string; target: string; has_archive: boolean;
  remote: boolean; remote_host?: string; versioning: boolean; delta: boolean; parallel?: number;
  include: string[]; exclude: string[];
  watch_interval_secs?: number; watch_auto_apply: boolean;
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
interface SideMeta { size: number; mtime_ms: number }
interface RowMeta { src: SideMeta | null; dst: SideMeta | null }
interface PlanDto {
  header: { mode: string; source_root: string; target_root: string; op_count: number; conflict_count: number; source_entries: number; target_entries: number };
  ops: OpDto[];
  reversed: (OpDto | null)[];
  /// 与 ops 一一对应的两侧实测 size/mtime（Rust 侧 compare::evidence 产出）
  metas: RowMeta[];
  equal_count: number;
  equal_bytes: number;
}
interface ApplyDto { done: number; skipped: number; errors: number; bytes_copied: number; cancelled: boolean }
/// M5：编辑器用的完整 Job（与 Rust config::Job 的 serde 形状一一对应）
interface JobFull {
  mode: string; source: string; target: string; archive?: string | null;
  include: string[]; exclude: string[]; no_hash: boolean; rigor: string;
  case_sensitive: boolean; symlinks: string; versioning: boolean;
  remote_host?: string | null; remote_root?: string | null; remote_exe?: string | null;
  require_marker: boolean; min_free_pct: number; max_delete_ratio: number;
  fsync: boolean; on_conflict: string; max_conflicts: number; sync_mode: boolean;
  deletable: string[]; delta: boolean; parallel?: number | null;
  watch_interval_secs?: number | null; watch_auto_apply: boolean;
}
interface RunRecord {
  ts_ms: number; job: string; kind: string;
  done: number; skipped: number; errors: number; bytes: number;
  elapsed_ms: number; cancelled: boolean; detail?: string;
}
interface Progress { phase: string; detail: string; pct: number; rate: number }
interface PreflightDto { ok: boolean; blockers: string[]; warnings: string[] }
interface PathInfo { exists: boolean; is_dir: boolean; has_marker: boolean }
interface PathVerdict { source: PathInfo; target: PathInfo; warnings: string[] }

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
const statusMsgEl = $('statusmsg');
const undoBtn = $<HTMLButtonElement>('btn-undo');
const modalEl = $('modal');
const modalBody = $('modal-body');
const modalOk = $('modal-ok') as HTMLButtonElement;

let jobs: JobDto[] = [];
let currentJob: JobDto | null = null;
let plan: PlanDto | null = null;
let checked: boolean[] = [];
let flipped: boolean[] = [];
/// 类别过滤：FFS 底部那三个大按钮是**各自独立的开关**，不是单选。
/// 空集 = 不过滤（对应"全部"高亮）
let chips = new Set<Chip>();
let search = '';
let busy = false;
/// M3 Overview：按顶层目录过滤（null = 不过滤；'(root)' = 根下散文件；'a' 或 'a/b' = 前缀）
let ovFilter: string | null = null;
let ovExpanded = new Set<string>();
/// M4：任务名 → 最近一次运行
let lastMap: Record<string, RunRecord> = {};
/// 路径显示模式：rel = 相对比对根目录（offset），full = 完整路径
type PathMode = 'rel' | 'full';
let pathMode: PathMode = localStorage.getItem('sd.pathmode') === 'full' ? 'full' : 'rel';
/// 树状分组（FFS 分组行语义）：连续同目录的行共享一条目录组头，文件行只显示文件名
let grouped = localStorage.getItem('sd.grouped') !== 'off';
const collapsedDirs = new Set<string>();
/// 用户在确认单里勾了"我确认无误"（等同 CLI --i-know）；每次重新比对后归零
let acknowledged = false;
/// 排序：null = 计划原序（walk 序）。分组算法依赖"同目录的行连续"，
/// 所以一旦排序就必须切平铺，两者互斥。
type SortKey = 'path' | 'action' | 's.size' | 's.mtime' | 't.size' | 't.mtime';
let sort: { key: SortKey; dir: 1 | -1 } | null = null;
/// P3 漏斗：作用于**当前比对结果**的视图过滤（不重扫）。
/// 掩码判定回 Rust（filter::mask_hits），大小/时间是纯数值留在前端。
interface ViewFilter { masks: string[]; minMB: number | null; maxMB: number | null; days: number | null }
let vfilter: ViewFilter = { masks: [], minMB: null, maxMB: null, days: null };
/// 与 plan.ops 一一对应的"被掩码命中"缓存；掩码或计划一变就作废
let maskHit: boolean[] = [];

// ---------- 小工具 ----------

function setStatus(msg: string, cls: '' | 'err' | 'ok' = '') {
  statusMsgEl.textContent = msg;
  statusEl.className = cls;
  undoBtn.classList.add('hidden');
  undoAction = null;
}

/// 带撤销的状态提示：任何"我替你改了任务文件"的动作都必须给一条回头路
let undoAction: (() => Promise<void>) | null = null;
function setStatusUndo(msg: string, label: string, fn: () => Promise<void>, cls: '' | 'err' | 'ok' = 'ok') {
  setStatus(msg, cls);
  undoAction = fn;
  undoBtn.textContent = label;
  undoBtn.classList.remove('hidden');
}
undoBtn.addEventListener('click', async () => {
  const fn = undoAction;
  if (!fn) return;
  undoBtn.classList.add('hidden');
  undoAction = null;
  try { await fn(); } catch (e) { setStatus(`撤销失败：${e}`, 'err'); }
});

function setBusy(b: boolean) {
  busy = b;
  spinEl.classList.toggle('hidden', !b);
  btnCompare.disabled = b || !currentJob;
  btnSync.disabled = b || !plan || plan.ops.length === 0;
  const sw = document.getElementById('btn-swap') as HTMLButtonElement | null;
  if (sw) sw.disabled = b || !currentJob;
}

function humanSize(b?: number): string {
  if (b === undefined) return '';
  if (b >= 1 << 30) return (b / (1 << 30)).toFixed(2) + ' GB';
  if (b >= 1 << 20) return (b / (1 << 20)).toFixed(1) + ' MB';
  if (b >= 1024) return (b / 1024).toFixed(1) + ' KB';
  return b + ' B';
}

/// 与 Rust 侧 MTIME_SLACK_MS 同值：FAT/SMB 的时间粒度，2 秒内不算"更新"
const MTIME_SLACK = 2000;

const p2 = (n: number) => String(n).padStart(2, '0');
/// 今年只显示 月-日 时:分，往年补上年份——列窄，信息密度优先
function fmtTime(ms: number): string {
  if (!ms) return '';
  const d = new Date(ms);
  const md = `${p2(d.getMonth() + 1)}-${p2(d.getDate())} ${p2(d.getHours())}:${p2(d.getMinutes())}`;
  return d.getFullYear() === new Date().getFullYear() ? md : `${d.getFullYear()}-${md}`;
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

function dirOf(p: string): string {
  const i = p.lastIndexOf('/');
  return i < 0 ? '' : p.slice(0, i);
}
function baseOf(p: string): string {
  const i = p.lastIndexOf('/');
  return i < 0 ? p : p.slice(i + 1);
}

function sepOf(root: string): string {
  return root.includes('\\') ? '\\' : '/';
}
function fullPath(root: string, rel: string): string {
  const sep = sepOf(root);
  const r = root.endsWith(sep) ? root.slice(0, -1) : root;
  return r + sep + (sep === '\\' ? rel.replace(/\//g, '\\') : rel);
}

/// 该行在 source / target 侧**现存**的路径（比对时点的状态，非执行后）：
/// copy 只存在于来源侧；delete 只存在于被删侧；move 的执行侧还叫 from、对面已是 path；
/// update/chmod/conflict/note 双侧都有。
function sidePaths(op: OpDto): [string | null, string | null] {
  const execOnTarget = op.side === 'target';
  switch (op.action) {
    case 'copy':
      return execOnTarget ? [op.path, null] : [null, op.path];
    case 'move': {
      const cur = op.from ?? op.path;
      return execOnTarget ? [op.path, cur] : [cur, op.path];
    }
    case 'delete':
    case 'delete_dir':
      return execOnTarget ? [null, op.path] : [op.path, null];
    default:
      return [op.path, op.path];
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

function metaOf(i: number): RowMeta {
  return plan?.metas?.[i] ?? { src: null, dst: null };
}

/// 排序取值。缺席的一侧排在最后（不管升降序），免得"没有的东西"抢占视线
function sortVal(i: number, key: SortKey): [number, string] {
  const op = eff(i);
  const m = metaOf(i);
  switch (key) {
    case 'path': return [0, op.path.toLowerCase()];
    case 'action': return [0, op.action];
    case 's.size': return [m.src ? 0 : 1, String(m.src?.size ?? 0).padStart(20, '0')];
    case 's.mtime': return [m.src ? 0 : 1, String(m.src?.mtime_ms ?? 0).padStart(20, '0')];
    case 't.size': return [m.dst ? 0 : 1, String(m.dst?.size ?? 0).padStart(20, '0')];
    case 't.mtime': return [m.dst ? 0 : 1, String(m.dst?.mtime_ms ?? 0).padStart(20, '0')];
  }
}

/// 这一行涉及的字节数与时间：两侧取较大/较新的那个（"这行牵扯到一个多大、多新的文件"）
function rowSize(i: number): number {
  const m = metaOf(i);
  return Math.max(eff(i).size ?? 0, m.src?.size ?? 0, m.dst?.size ?? 0);
}
function rowMtime(i: number): number {
  const m = metaOf(i);
  return Math.max(m.src?.mtime_ms ?? 0, m.dst?.mtime_ms ?? 0);
}

/// 漏斗判定：命中掩码 = 隐藏；大小/时间在范围外 = 隐藏
function passesFunnel(i: number): boolean {
  if (maskHit[i]) return false;
  const MB = 1024 * 1024;
  if (vfilter.minMB !== null || vfilter.maxMB !== null) {
    const sz = rowSize(i);
    if (vfilter.minMB !== null && sz < vfilter.minMB * MB) return false;
    if (vfilter.maxMB !== null && sz > vfilter.maxMB * MB) return false;
  }
  if (vfilter.days !== null) {
    const t = rowMtime(i);
    if (!t || Date.now() - t > vfilter.days * 86400_000) return false;
  }
  return true;
}

function funnelActive(): number {
  return (vfilter.masks.length ? 1 : 0)
    + (vfilter.minMB !== null || vfilter.maxMB !== null ? 1 : 0)
    + (vfilter.days !== null ? 1 : 0);
}

function visibleIdx(): number[] {
  if (!plan) return [];
  const out: number[] = [];
  plan.ops.forEach((_, i) => {
    const op = eff(i);
    if ((chips.size === 0 || chips.has(category(op))) && matchesSearch(op) && matchesOv(op) && passesFunnel(i)) out.push(i);
  });
  if (sort) {
    const { key, dir } = sort;
    out.sort((a, b) => {
      const [ma, va] = sortVal(a, key);
      const [mb, vb] = sortVal(b, key);
      if (ma !== mb) return ma - mb; // 缺席恒排后
      return va < vb ? -dir : va > vb ? dir : a - b;
    });
  }
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
    const on = key === 'all' ? chips.size === 0 : chips.has(key);
    const b = document.createElement('button');
    b.className = 'chip' + (on ? ' on' : '') + (n === 0 ? ' zero' : '');
    b.textContent = `${label} ${n}`;
    b.title = key === 'all' ? '清除类别过滤' : '这一类的开关（可与其它类同时打开）';
    b.addEventListener('click', () => {
      if (key === 'all') chips.clear();
      else if (chips.has(key)) chips.delete(key);
      else chips.add(key);
      renderAll();
    });
    chipsEl.appendChild(b);
  }
}

/// M3：图标化统计条（FFS 底部统计条同款语义：0 值置灰、非 0 加粗）
function renderStats() {
  if (!plan) { statsEl.textContent = ''; return; }
  const cnt = { copy: 0, upd: 0, mv: 0, del: 0 };
  let bytes = 0;
  // 统计条口径 = 真正会执行的那一批（勾选 ∩ 可见），与确认单一致
  finalIdx().forEach((i) => {
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
      `<div class="ovbar"><div></div></div>`;
    // 宽度走 CSSOM：style="" 属性会被 Tauri 注入 nonce 后的 CSP 拦掉，JS 赋值不受管
    (row.querySelector('.ovbar > div') as HTMLElement).style.width = `${pct}%`;
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

/// 单个 op 行（groupDir = 树状分组时所在组目录；组内文件只显示文件名，
/// 跨目录的 move 来源自动保留完整相对路径以免信息丢失）
function buildRow(i: number, groupDir: string | null): HTMLTableRowElement {
  const p = plan!;
  const op = eff(i);
  const canFlip = !!p.reversed[i] && selectable(p.ops[i]);
  const tr = document.createElement('tr');
  if (!checked[i]) tr.classList.add('off');
  if (flipped[i]) tr.classList.add('flip');
  if (groupDir !== null) tr.classList.add('ingrp');

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

  // 左右双路径列（FFS 双栏语义）；tooltip 恒为完整路径
  const [sp, tp] = sidePaths(op);
  const mkPath = (pv: string | null, root: string) => {
    const td = document.createElement('td');
    td.className = 'mono c-path';
    if (pv) {
      td.textContent =
        groupDir !== null && dirOf(pv) === groupDir
          ? baseOf(pv)
          : pathMode === 'full' ? fullPath(root, pv) : pv;
      td.title = fullPath(root, pv);
    } else {
      td.classList.add('dim');
    }
    return td;
  };
  const tdPath = mkPath(sp, p.header.source_root);
  const tdFrom = mkPath(tp, p.header.target_root);

  // 双侧实测 size/时间（FFS 的两组 date/size 列）：较新的一侧染色，
  // "哪边新"是审阅时最常问的一句，今天只能去 reason 里猜
  const m = metaOf(i);
  const newer = m.src && m.dst
    ? (m.src.mtime_ms - m.dst.mtime_ms > MTIME_SLACK ? 's' : m.dst.mtime_ms - m.src.mtime_ms > MTIME_SLACK ? 't' : '')
    : '';
  const mkMeta = (sm: SideMeta | null, isNewer: boolean) => {
    const td = document.createElement('td');
    td.className = 'c-meta mono' + (isNewer ? ' newer' : '');
    if (sm) {
      td.textContent = `${humanSize(sm.size)} · ${fmtTime(sm.mtime_ms)}`;
      td.title = `${sm.size.toLocaleString()} 字节\n${new Date(sm.mtime_ms).toLocaleString()}`;
    } else {
      td.classList.add('dim');
      td.textContent = '—';
    }
    return td;
  };

  const tdReason = document.createElement('td');
  tdReason.className = 'reason';
  tdReason.textContent = op.reason;

  tr.append(tdChk, tdAct, tdPath, mkMeta(m.src, newer === 's'), tdFrom, mkMeta(m.dst, newer === 't'), tdReason);
  tr.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    rowMenu(i, e.clientX, e.clientY);
  });
  return tr;
}

/// 目录组头行：▾/▸ 目录 · N 项 · 合计体积 + 整组勾选框（三态）
function buildGroupRow(dir: string, items: number[]): HTMLTableRowElement {
  const p = plan!;
  const tr = document.createElement('tr');
  tr.className = 'grp';
  const selectableItems = items.filter((i) => selectable(eff(i)));
  const nChecked = selectableItems.filter((i) => checked[i]).length;

  const tdChk = document.createElement('td');
  tdChk.className = 'c-chk';
  const cb = document.createElement('input');
  cb.type = 'checkbox';
  cb.checked = selectableItems.length > 0 && nChecked === selectableItems.length;
  cb.indeterminate = nChecked > 0 && nChecked < selectableItems.length;
  cb.disabled = selectableItems.length === 0;
  cb.title = '勾选/取消整个目录';
  cb.addEventListener('click', (e) => e.stopPropagation());
  cb.addEventListener('change', () => {
    for (const i of selectableItems) checked[i] = cb.checked;
    renderTable();
  });
  tdChk.appendChild(cb);

  const td = document.createElement('td');
  td.colSpan = 6;
  const folded = collapsedDirs.has(dir);
  let bytes = 0;
  for (const i of items) bytes += eff(i).size ?? 0;
  const label = dir === '' ? '(根目录)' : dir;
  td.innerHTML =
    `<span class="gchev">${folded ? '▸' : '▾'}</span> <span class="gdir mono">${escapeHtml(label)}</span>` +
    `<span class="gmeta">${items.length} 项${bytes ? ` · ${humanSize(bytes)}` : ''}</span>`;
  td.title = `${plan!.header.source_root}\n${p.header.target_root}\n… ${label}`;
  tr.appendChild(td);
  tr.addEventListener('click', () => {
    if (collapsedDirs.has(dir)) collapsedDirs.delete(dir);
    else collapsedDirs.add(dir);
    renderModeButtons();
    renderTable();
  });
  return tr;
}

let lastGroupDirs: string[] = [];

function renderTable() {
  bodyEl.innerHTML = '';
  const has = !!plan && plan.ops.length > 0;
  filterBar.classList.toggle('hidden', !has);
  tableEl.classList.toggle('hidden', !has);
  emptyEl.classList.toggle('hidden', has);
  if (!plan) { emptyEl.textContent = '← 选择任务，然后 Compare（Ctrl+R）'; return; }
  if (plan.ops.length === 0) { emptyEl.textContent = '✓ 两侧一致，没有需要同步的内容'; return; }

  const vis = visibleIdx();
  // 排序中一律平铺：分组靠"同目录的行在计划里连续"这条不变量，排完就没有了
  if (!grouped || sort) {
    lastGroupDirs = [];
    for (const i of vis) bodyEl.appendChild(buildRow(i, null));
  } else {
    // 连续同目录成组（计划本身按 walk 序，过滤后依然是连续子序列）
    const groups: { dir: string; items: number[] }[] = [];
    for (const i of vis) {
      const d = dirOf(eff(i).path);
      if (!groups.length || groups[groups.length - 1].dir !== d) groups.push({ dir: d, items: [] });
      groups[groups.length - 1].items.push(i);
    }
    lastGroupDirs = [...new Set(groups.map((g) => g.dir))];
    for (const g of groups) {
      bodyEl.appendChild(buildGroupRow(g.dir, g.items));
      if (!collapsedDirs.has(g.dir)) {
        for (const i of g.items) bodyEl.appendChild(buildRow(i, g.dir));
      }
    }
  }
  syncHeadCheckbox();
  renderStats();
  renderCounts();
  renderSortMarks();
}

/// FFS 状态条的那句 "Showing 481 of 23,112 items"：
/// 显示数 / 计划数 / 两侧已扫描 / 判定相等。缺了它，一个文件没出现在表里时
/// 你分不清它是"相等"还是"根本没扫到"。
function renderCounts() {
  const el = document.getElementById('counts');
  if (!el) return;
  if (!plan) { el.textContent = ''; return; }
  const vis = visibleIdx().length;
  const tot = plan.ops.length;
  const h = plan.header;
  const parts = [`显示 ${vis} / ${tot}`];
  if (vis < tot) parts.push(`隐藏 ${tot - vis} 不执行`);
  parts.push(`已扫描 ${h.source_entries.toLocaleString()} ⇄ ${h.target_entries.toLocaleString()}`);
  parts.push(`相同 ${(plan.equal_count ?? 0).toLocaleString()}`);
  el.textContent = parts.join(' · ');
  el.title = `计划 ${tot} 项；两侧相同 ${(plan.equal_count ?? 0).toLocaleString()} 个文件（${humanSize(plan.equal_bytes ?? 0)}）`;
}

function renderSortMarks() {
  for (const el of document.querySelectorAll<HTMLElement>('#plantable th .sortable')) {
    const on = !!sort && el.dataset.sort === sort.key;
    el.classList.toggle('on', on);
    el.dataset.dir = on ? (sort!.dir === 1 ? '▲' : '▼') : '';
  }
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

function relTime(ts: number): string {
  const d = Date.now() - ts;
  if (d < 60_000) return '刚刚';
  if (d < 3600_000) return `${Math.floor(d / 60_000)} 分钟前`;
  if (d < 86400_000) return `${Math.floor(d / 3600_000)} 小时前`;
  return `${Math.floor(d / 86400_000)} 天前`;
}

function renderJobs() {
  jobListEl.innerHTML = '';
  for (const j of jobs) {
    const div = document.createElement('div');
    div.className = 'job' + (currentJob?.name === j.name ? ' active' : '');
    const rigor = j.rigor && j.rigor !== 'standard' ? `<span class="rigor">${j.rigor}</span>` : '';
    const remote = j.remote ? `<span class="rbadge">ssh</span>` : '';
    // M4：上次同步行（结果色点 + 相对时间；超 7 天变红提醒）
    const r = lastMap[j.name];
    let sub = '';
    if (r) {
      const dot = r.errors > 0 ? 'err' : r.cancelled ? 'warn' : 'ok';
      const stale = Date.now() - r.ts_ms > 7 * 86400_000 ? ' stale' : '';
      const note = r.errors > 0 ? ` · ${r.errors} 错误` : r.cancelled ? ' · 已取消' : '';
      sub = `<div class="jrow2${stale}"><span class="dot ${dot}">●</span>${relTime(r.ts_ms)} · ${r.done} 项${note}</div>`;
    }
    div.innerHTML = `<div class="jrow1"><span class="name">${j.name}</span>${remote}${rigor}<span class="mode ${j.mode}">${j.mode}</span><button class="jedit" title="编辑任务">✎</button></div>${sub}`;
    div.title = `${j.source}\n→ ${j.target}` + (j.remote ? `\nssh:${j.remote_host ?? ''}` : '');
    (div.querySelector('.jedit') as HTMLButtonElement).addEventListener('click', (e) => {
      e.stopPropagation();
      openEditor(j.name);
    });
    div.addEventListener('click', () => {
      if (busy) return;
      currentJob = j;
      plan = null; checked = []; flipped = []; chips.clear(); search = ''; searchEl.value = '';
      ovFilter = null; ovExpanded.clear(); sort = null;
      vfilter = { masks: [], minMB: null, maxMB: null, days: null }; maskHit = [];
      renderFunnelBtn();
      renderJobs();
      renderAll();
      pathEl.textContent = `${j.source}   ⇄   ${j.target}`;
      btnCompare.disabled = false;
      btnSync.disabled = true;
      btnWatch.disabled = false;
      renderVariants();
      watchStop();
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
  // v0.9.1 第一性重设计：compare 进度原地显示在差异表区域——主窗本来就在眼前，
  // 不再弹独立窗口（小目录不闪窗、大目录有实时双侧计数、且规避子窗生命周期问题）
  cmpShow();
  try {
    acknowledged = false;
    modalOk.disabled = false;
    plan = await invoke<PlanDto>('compare_job', { name: currentJob.name });
    checked = plan.ops.map((op) => selectable(op));
    flipped = plan.ops.map(() => false);
    chips.clear();
    ovFilter = null; ovExpanded.clear(); sort = null;
    // 新计划 = 新的行集合，掩码缓存作废；漏斗条件本身保留（连续几轮盯同一批文件很常见）
    await recomputeMasks();
    renderFunnelBtn();
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
  cmpHide();
  setBusy(false);
}

/// 最终动作集 = 勾选 ∩ 当前可见。
/// FFS 的语义是"视图即动作集"：看不见的东西不会被执行。这也堵上了旧行为里
/// 一个安静的坑——过去搜索框一过滤，被隐藏的行照样跟着 Synchronize 跑掉。
function finalIdx(): number[] {
  const vis = new Set(visibleIdx());
  return checked.map((c, i) => (c && vis.has(i) ? i : -1)).filter((i) => i >= 0);
}

async function openConfirm() {
  if (!currentJob || !plan || busy) return;
  const idx = finalIdx();
  const hiddenChecked = checked.filter(Boolean).length - idx.length;
  if (idx.length === 0) {
    setStatus(hiddenChecked > 0 ? '勾选的行全被过滤器隐藏了 —— 先清除筛选' : '没有勾选任何项', 'err');
    return;
  }
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
    ${hiddenChecked > 0 ? `<div class="mrow warn"><span>被筛选隐藏，不执行</span><b>${hiddenChecked}</b><span class="dim">视图即动作集</span></div>` : ''}
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
  const idx = finalIdx();
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
    refreshLastSyncs();
    await doCompare();
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

// ---------- M5：任务编辑器（schema 驱动，字段与 config::Job 对齐） ----------

const editModal = $('editmodal');
const edForm = $('ed-form');
const edDelete = $<HTMLButtonElement>('ed-delete');
let edName: string | null = null; // 正在编辑的任务名（null = 新建）

type FKind = 'text' | 'num' | 'bool' | 'select' | 'lines' | 'dir' | 'file';
interface FSpec { key: string; label: string; kind: FKind; opts?: string[]; hint?: string; wide?: boolean; group?: string }
const ED_FIELDS: FSpec[] = [
  { key: '__name', label: '任务名（文件名）', kind: 'text', group: '基本' },
  { key: 'mode', label: '模式', kind: 'select', opts: ['mirror', 'sync', 'enrich'] },
  { key: 'source', label: 'source 根目录', kind: 'dir', wide: true },
  { key: 'target', label: 'target 根目录', kind: 'dir', wide: true },
  { key: 'archive', label: 'archive 存档文件（sync 模式）', kind: 'file', hint: '留空 = 无；建议 %APPDATA%\\syncdash\\archives\\<名>.jsonl', wide: true },
  { key: 'rigor', label: '严谨级', kind: 'select', opts: ['quick', 'fast', 'standard', 'paranoid'], hint: 'fast=抽样摘要：大文件只读头/中/尾各256KB，比quick多内容防线、比standard快百倍（云盘/媒体库推荐）' },
  { key: 'symlinks', label: 'symlink 策略', kind: 'select', opts: ['exclude', 'direct'] },
  { key: 'case_sensitive', label: '大小写敏感比对', kind: 'bool' },
  { key: 'versioning', label: '版本控制（.version_syncDash）', kind: 'bool', group: '行为' },
  { key: 'delta', label: '本地增量写（delta）', kind: 'bool' },
  { key: 'fsync', label: 'rename 前 fsync', kind: 'bool' },
  { key: 'sync_mode', label: '同步 unix 权限位', kind: 'bool' },
  { key: 'parallel', label: '并行宽度（空=4）', kind: 'num' },
  { key: 'on_conflict', label: '冲突策略', kind: 'select', opts: ['report', 'copy', 'newer'] },
  { key: 'max_conflicts', label: '冲突副本上限', kind: 'num' },
  { key: 'require_marker', label: '要求 .syncdash-root 标记', kind: 'bool', group: '守护' },
  { key: 'min_free_pct', label: '最低空闲盘比例（0.01=1%）', kind: 'num' },
  { key: 'max_delete_ratio', label: '删除占比闸门（0.5=50%）', kind: 'num' },
  { key: 'include', label: 'include（每行一条）', kind: 'lines', group: '过滤', wide: true },
  { key: 'exclude', label: 'exclude（每行一条；! 开头 = 例外）', kind: 'lines', wide: true },
  { key: 'deletable', label: 'deletable（删父目录可连带删）', kind: 'lines', wide: true },
  { key: 'remote_host', label: 'ssh 主机别名', kind: 'text', group: '远程（可选）' },
  { key: 'remote_root', label: '远端根路径', kind: 'text' },
  { key: 'remote_exe', label: '远端 syncdash 路径（空=PATH）', kind: 'text' },
  { key: 'watch_interval_secs', label: '定时扫描间隔（秒；空=关）', kind: 'num', group: '值守（Watch）', hint: '秒级=准实时；UNC 目标建议 ≥30' },
  { key: 'watch_auto_apply', label: '发现差异自动执行', kind: 'bool' },
];

function defaultJob(): JobFull {
  return {
    mode: 'mirror', source: '', target: '', archive: null,
    include: [], exclude: [], no_hash: false, rigor: 'standard',
    case_sensitive: false, symlinks: 'exclude', versioning: false,
    remote_host: null, remote_root: null, remote_exe: null,
    require_marker: false, min_free_pct: 0.01, max_delete_ratio: 0.5,
    fsync: true, on_conflict: 'report', max_conflicts: 5, sync_mode: false,
    deletable: [], delta: false, parallel: null,
    watch_interval_secs: null, watch_auto_apply: false,
  };
}

// ---------- P1：路径选择器 / 历史 / 体检 ----------

/// 原生对话框。@tauri-apps/plugin-dialog 那个 npm 包只是这一行 invoke 的包装，
/// 直接调 IPC 省一个前端依赖（Rust 侧已注册 tauri_plugin_dialog）。
async function pickPath(opts: { directory?: boolean; save?: boolean; title: string; defaultPath?: string }): Promise<string | null> {
  const { directory, save, title, defaultPath } = opts;
  try {
    const r = await invoke<unknown>(save ? 'plugin:dialog|save' : 'plugin:dialog|open', {
      options: { title, defaultPath: defaultPath || undefined, directory: !!directory, multiple: false, recursive: false },
    });
    if (!r) return null;
    // open 在不同小版本里可能回 string | string[] | {path}
    const one = Array.isArray(r) ? r[0] : r;
    if (typeof one === 'string') return one;
    if (one && typeof one === 'object' && typeof (one as { path?: string }).path === 'string') return (one as { path: string }).path;
    return null;
  } catch (e) {
    setStatus(`打不开选择器：${e}`, 'err');
    return null;
  }
}

const HIST_KEY = 'sd.pathhist';
function pathHistory(): string[] {
  try { return JSON.parse(localStorage.getItem(HIST_KEY) ?? '[]') as string[]; } catch { return []; }
}
function pushHistory(p: string) {
  const v = p.trim();
  if (!v) return;
  const list = [v, ...pathHistory().filter((x) => x.toLowerCase() !== v.toLowerCase())].slice(0, 12);
  localStorage.setItem(HIST_KEY, JSON.stringify(list));
}

function edInput(key: string): HTMLInputElement | null {
  return edForm.querySelector<HTMLInputElement>(`input[data-k="${key}"]`);
}

let verdictTimer: number | null = null;
/// 路径体检（去抖）：存不存在、是不是目录、两根是否相同/嵌套。
/// 写错根目录的代价太大，不该等到 Compare 才知道。
function scheduleVerdict() {
  if (verdictTimer !== null) clearTimeout(verdictTimer);
  verdictTimer = window.setTimeout(async () => {
    const box = document.getElementById('ed-verdict');
    if (!box) return;
    const s = edInput('source')?.value ?? '';
    const t = edInput('target')?.value ?? '';
    if (!s && !t) { box.innerHTML = ''; return; }
    try {
      const v = await invoke<PathVerdict>('inspect_paths', { source: s, target: t });
      const mark = (el: HTMLInputElement | null, info: PathInfo, val: string) => {
        if (!el) return;
        el.classList.toggle('bad', !!val.trim() && !info.is_dir);
        el.classList.toggle('good', info.is_dir);
      };
      mark(edInput('source'), v.source, s);
      mark(edInput('target'), v.target, t);
      const marks = [
        v.source.has_marker ? 'source 有 .syncdash-root 标记' : '',
        v.target.has_marker ? 'target 有 .syncdash-root 标记' : '',
      ].filter(Boolean).join(' · ');
      box.innerHTML =
        v.warnings.map((w) => `<div class="vwarn">⚠ ${escapeHtml(w)}</div>`).join('') +
        (marks ? `<div class="vok">✓ ${escapeHtml(marks)}</div>` : '');
    } catch { /* 体检失败不该挡住编辑 */ }
  }, 300);
}

function edBuild(j: JobFull, name: string) {
  edForm.innerHTML = '';
  // 路径历史走原生 datalist：键盘可用、零自定义弹层代码
  const dl = document.createElement('datalist');
  dl.id = 'sd-paths';
  for (const p of pathHistory()) {
    const o = document.createElement('option');
    o.value = p;
    dl.appendChild(o);
  }
  edForm.appendChild(dl);
  for (const f of ED_FIELDS) {
    if (f.group) {
      const g = document.createElement('div');
      g.className = 'ed-group';
      g.textContent = f.group;
      edForm.appendChild(g);
    }
    const wrap = document.createElement('label');
    wrap.className = 'ed-field' + (f.wide ? ' wide' : '') + (f.kind === 'bool' ? ' ed-check' : '');
    const v: unknown = f.key === '__name' ? name : (j as unknown as Record<string, unknown>)[f.key];
    let inner = '';
    if (f.kind === 'select') {
      inner = `<span>${f.label}</span><select data-k="${f.key}">` +
        f.opts!.map((o) => `<option${o === v ? ' selected' : ''}>${o}</option>`).join('') + '</select>';
    } else if (f.kind === 'bool') {
      inner = `<input type="checkbox" data-k="${f.key}"${v ? ' checked' : ''}/><span>${f.label}</span>`;
    } else if (f.kind === 'lines') {
      inner = `<span>${f.label}</span><textarea data-k="${f.key}" spellcheck="false">${escapeHtml(((v as string[]) ?? []).join('\n'))}</textarea>`;
    } else if (f.kind === 'num') {
      inner = `<span>${f.label}</span><input type="number" step="any" data-k="${f.key}" value="${v ?? ''}"/>`;
    } else if (f.kind === 'dir' || f.kind === 'file') {
      const swap = f.key === 'source'
        ? `<button type="button" class="pbtn" data-swap="1" title="与 target 对调">⇄</button>` : '';
      inner = `<span>${f.label}</span><div class="pathrow">` +
        `<input type="text" data-k="${f.key}" data-drop="1" list="sd-paths" value="${escapeHtml(String(v ?? ''))}" spellcheck="false"/>` +
        swap +
        `<button type="button" class="pbtn" data-pick="${f.kind}" data-for="${f.key}" title="浏览…">📁</button></div>`;
    } else {
      inner = `<span>${f.label}</span><input type="text" data-k="${f.key}" value="${escapeHtml(String(v ?? ''))}" spellcheck="false"/>`;
    }
    if (f.hint) inner += `<span class="thin">${f.hint}</span>`;
    wrap.innerHTML = inner;
    edForm.appendChild(wrap);
    // 两个根目录之后立刻贴体检结果，警告紧挨着它们说的那两个字段
    if (f.key === 'target') {
      const box = document.createElement('div');
      box.id = 'ed-verdict';
      box.className = 'ed-verdict';
      edForm.appendChild(box);
    }
  }
  wireEditorPaths();
}

/// 给刚生成的表单接线：浏览按钮、⇄ 对调、输入触发体检
function wireEditorPaths() {
  for (const b of edForm.querySelectorAll<HTMLButtonElement>('button[data-pick]')) {
    b.addEventListener('click', async (e) => {
      e.preventDefault();
      const key = b.dataset.for!;
      const el = edInput(key);
      if (!el) return;
      const isDir = b.dataset.pick === 'dir';
      const p = await pickPath({
        directory: isDir,
        save: !isDir,
        title: isDir ? '选择目录' : '选择存档文件',
        defaultPath: el.value.trim(),
      });
      if (!p) return;
      el.value = p;
      if (isDir) pushHistory(p);
      scheduleVerdict();
    });
  }
  const sw = edForm.querySelector<HTMLButtonElement>('button[data-swap]');
  if (sw) {
    sw.addEventListener('click', (e) => {
      e.preventDefault();
      const s = edInput('source'), t = edInput('target');
      if (!s || !t) return;
      const tmp = s.value;
      s.value = t.value;
      t.value = tmp;
      scheduleVerdict();
    });
  }
  for (const el of edForm.querySelectorAll<HTMLInputElement>('input[data-drop]')) {
    el.addEventListener('input', scheduleVerdict);
  }
  scheduleVerdict();
}

async function openEditor(name?: string, focusGroup?: string) {
  edName = name ?? null;
  $('ed-title').textContent = name ? `编辑任务 — ${name}` : '新任务';
  edDelete.classList.toggle('hidden', !name);
  let j = defaultJob();
  if (name) {
    try { j = await invoke<JobFull>('get_job', { name }); } catch (e) { setStatus(`读取任务失败：${e}`, 'err'); return; }
  }
  edBuild(j, name ?? '');
  editModal.classList.remove('hidden');
  if (focusGroup) {
    for (const g of edForm.querySelectorAll<HTMLElement>('.ed-group')) {
      if (g.textContent === focusGroup) { g.scrollIntoView({ block: 'start' }); break; }
    }
  }
}

function edCollect(): { name: string; job: JobFull } | null {
  const j = defaultJob() as unknown as Record<string, unknown>;
  let name = '';
  for (const el of edForm.querySelectorAll<HTMLElement>('[data-k]')) {
    const k = el.dataset.k!;
    let val: unknown;
    if (el instanceof HTMLInputElement && el.type === 'checkbox') val = el.checked;
    else if (el instanceof HTMLInputElement && el.type === 'number') val = el.value.trim() === '' ? null : Number(el.value);
    else if (el instanceof HTMLTextAreaElement) val = el.value.split('\n').map((s) => s.trim()).filter(Boolean);
    else val = (el as HTMLInputElement | HTMLSelectElement).value.trim();
    if (k === '__name') { name = String(val); continue; }
    // 空字符串的可选项落成 null（serde Option）
    if ((k === 'archive' || k === 'remote_host' || k === 'remote_root' || k === 'remote_exe') && val === '') val = null;
    j[k] = val;
  }
  if (!name) { setStatus('任务名不能为空', 'err'); return null; }
  const jf = j as unknown as JobFull;
  if (!jf.source || !jf.target) { setStatus('source / target 不能为空', 'err'); return null; }
  return { name, job: jf };
}

$('btn-newjob').addEventListener('click', () => openEditor());
$('ed-cancel').addEventListener('click', () => editModal.classList.add('hidden'));
editModal.addEventListener('click', (e) => { if (e.target === editModal) editModal.classList.add('hidden'); });
$('ed-save').addEventListener('click', async () => {
  const c = edCollect();
  if (!c) return;
  try {
    await invoke('save_job', { name: c.name, job: c.job });
    editModal.classList.add('hidden');
    pushHistory(c.job.source);
    pushHistory(c.job.target);
    jobs = await invoke<JobDto[]>('list_jobs');
    renderJobs();
    // 改的就是当前选中的任务时，工具栏副标题与路径行要跟着变
    if (currentJob?.name === c.name) {
      currentJob = jobs.find((x) => x.name === c.name) ?? currentJob;
      pathEl.textContent = `${currentJob.source}   ⇄   ${currentJob.target}`;
      renderVariants();
    }
    setStatus(`已保存 '${c.name}'`, 'ok');
  } catch (e) {
    setStatus(`保存失败：${e}`, 'err');
  }
});
edDelete.addEventListener('click', async () => {
  if (!edName) return;
  if (!confirm(`删除任务配置 '${edName}'？（只删 TOML，不动任何数据）`)) return;
  try {
    await invoke('delete_job', { name: edName });
    editModal.classList.add('hidden');
    if (currentJob?.name === edName) { currentJob = null; plan = null; renderAll(); }
    jobs = await invoke<JobDto[]>('list_jobs');
    renderJobs();
    setStatus(`已删除 '${edName}'`);
  } catch (e) {
    setStatus(`删除失败：${e}`, 'err');
  }
});

// ---------- M6：值守（定时扫描；秒级=准实时） ----------

const btnWatch = $<HTMLButtonElement>('btn-watch');
let watchTimer: number | null = null;
let watchNext = 0;

function watchStop() {
  if (watchTimer !== null) { clearInterval(watchTimer); watchTimer = null; }
  btnWatch.classList.remove('on');
  btnWatch.textContent = '⏱ Watch';
}

async function watchTick() {
  if (!currentJob) { watchStop(); return; }
  const iv = (currentJob.watch_interval_secs ?? 30) * 1000;
  const left = watchNext - Date.now();
  if (left > 0) {
    if (!busy) setStatus(`⏱ 值守中 — ${Math.ceil(left / 1000)}s 后扫描（${currentJob.name}）`);
    return;
  }
  if (busy) return; // 上一轮还没完，跳过这拍
  watchNext = Date.now() + iv;
  await doCompare();
  if (plan && plan.ops.length > 0) {
    if (currentJob.watch_auto_apply) {
      const finalOps = plan.ops.filter((op) => selectable(op));
      setStatus(`⏱ 值守发现 ${finalOps.length} 项差异 — 自动执行中…`);
      setBusy(true);
      try {
        const r = await invoke<ApplyDto>('apply_job', { name: currentJob.name, plan, ops: finalOps, acknowledged: false });
        setStatus(`⏱ 自动同步完成：${r.done} 执行，${r.errors} 错误`, r.errors ? 'err' : 'ok');
        refreshLastSyncs();
      } catch (e) {
        setStatus(`⏱ 自动同步失败：${e}`, 'err');
      }
      setBusy(false);
    } else {
      setStatus(`⏱ 值守发现 ${plan.ops.length} 项差异 — 审阅后 Synchronize`, 'err');
    }
  }
}

btnWatch.addEventListener('click', () => {
  if (watchTimer !== null) { watchStop(); setStatus('值守已停止'); return; }
  if (!currentJob) return;
  const iv = currentJob.watch_interval_secs ?? 30;
  watchNext = Date.now() + iv * 1000;
  watchTimer = window.setInterval(watchTick, 1000);
  btnWatch.classList.add('on');
  btnWatch.textContent = `⏱ Watch ${iv}s`;
  setStatus(`⏱ 值守已开启：每 ${iv}s 比对一次（hash 缓存让未变的树只付 walk 成本）${currentJob.watch_auto_apply ? ' · 自动执行' : ''}`);
});

// ---------- M4：运行日志面板 ----------

const logModal = $('logmodal');
const logList = $('log-list');
const logDetail = $('log-detail');
const logBack = $<HTMLButtonElement>('log-back');

async function refreshLastSyncs() {
  try {
    lastMap = await invoke<Record<string, RunRecord>>('last_syncs');
    renderJobs();
  } catch { /* 日志缺失不致命 */ }
}

function logShowList() {
  logDetail.classList.add('hidden');
  logBack.classList.add('hidden');
  logList.classList.remove('hidden');
}

async function openLogPanel() {
  $('log-scope').textContent = currentJob ? `— ${currentJob.name}` : '— 全部任务';
  logShowList();
  logList.innerHTML = '<div class="logempty">加载中…</div>';
  logModal.classList.remove('hidden');
  try {
    const rows = await invoke<RunRecord[]>('run_history', { job: currentJob?.name ?? null, limit: 50 });
    logList.innerHTML = '';
    if (rows.length === 0) {
      logList.innerHTML = '<div class="logempty">还没有运行记录 — 任务真正执行（apply）后才会留痕</div>';
      return;
    }
    for (const r of rows) {
      const div = document.createElement('div');
      div.className = 'logrow';
      const dot = r.errors > 0 ? 'err' : r.cancelled ? 'warn' : 'ok';
      div.innerHTML =
        `<span><span class="dot ${dot}">●</span> ${relTime(r.ts_ms)}</span>` +
        `<span>${escapeHtml(r.job)} <span class="k">${r.kind}</span></span>` +
        `<span>${r.done} 项${r.errors ? ` · <b style="color:var(--red)">${r.errors} 错</b>` : ''}</span>` +
        `<span>${humanSize(r.bytes) || '0 B'} · ${(r.elapsed_ms / 1000).toFixed(1)}s${r.cancelled ? ' · 已取消' : ''}</span>` +
        `<span class="k">${r.detail ? '点击看明细 ›' : ''}</span>`;
      if (r.detail) {
        div.addEventListener('click', async () => {
          const lines = await invoke<string[]>('run_detail', { detail: r.detail });
          logDetail.textContent = lines.length ? lines.join('\n') : '(明细文件为空或已被清理)';
          logList.classList.add('hidden');
          logDetail.classList.remove('hidden');
          logBack.classList.remove('hidden');
        });
      }
      logList.appendChild(div);
    }
  } catch (e) {
    logList.innerHTML = `<div class="logempty">读取失败：${escapeHtml(String(e))}</div>`;
  }
}

$('btn-log').addEventListener('click', openLogPanel);
logBack.addEventListener('click', logShowList);
$('log-close').addEventListener('click', () => logModal.classList.add('hidden'));
logModal.addEventListener('click', (e) => { if (e.target === logModal) logModal.classList.add('hidden'); });

// ---------- v0.9.1：主窗内嵌 compare 进度面板（数据源 = run-progress 事件流） ----------

interface CmpEv {
  kind: string;
  purpose?: string;
  phase?: string;
  label?: string | null;
  ts_ms?: number;
  items_done?: number;
  items_total?: number;
  bytes_done?: number;
  bytes_total?: number;
}

const CMP_LABEL: Record<string, string> = {
  'scan-source': '扫描 source', 'scan-target': '扫描 target', 'compare': '比对', 'refresh': '刷新存档',
};
const cmpPanel = $('cmp-panel');
const cmpRows = $('cmp-rows');
const cmpCancelBtn = $<HTMLButtonElement>('cmp-cancel');
let cmpActive = false;
/// 速率 EMA（0.7 旧 + 0.3 新）：瞬时速率会随文件大小剧烈跳变，指数平滑后可读
const cmpRate = new Map<string, { t: number; b: number; ema: number }>();

function cmpShow() {
  cmpActive = true;
  cmpRows.innerHTML = '';
  cmpRate.clear();
  cmpCancelBtn.disabled = false;
  cmpCancelBtn.textContent = '✕ 取消';
  emptyEl.classList.add('hidden');
  tableEl.classList.add('hidden');
  filterBar.classList.add('hidden');
  cmpPanel.classList.remove('hidden');
}

function cmpHide() {
  cmpActive = false;
  cmpPanel.classList.add('hidden');
  // 表格/空态的显隐交还给 renderTable（doCompare 的 renderAll 已调用）
}

interface CmpRowRefs {
  row: HTMLElement; ico: HTMLElement; bar: HTMLElement; pct: HTMLElement;
  items: HTMLElement; bytes: HTMLElement; rate: HTMLElement;
}

/// 定宽网格行：数字全右对齐、等宽数字字体、进度条+百分比——杜绝整行来回跳变
function cmpRow(phase: string): CmpRowRefs {
  const id = `cmp-r-${phase}`;
  let row = document.getElementById(id);
  if (!row) {
    row = document.createElement('div');
    row.id = id;
    row.className = 'stagerow cmp2';
    row.innerHTML =
      `<span class="st-ico">⟳</span><span class="st-name">${CMP_LABEL[phase] ?? phase}</span>` +
      `<span class="st-bar"><i></i></span><span class="st-pct"></span>` +
      `<span class="st-items"></span><span class="st-bytes"></span><span class="st-rate"></span>`;
    cmpRows.appendChild(row);
  }
  return {
    row,
    ico: row.querySelector('.st-ico') as HTMLElement,
    bar: row.querySelector('.st-bar > i') as HTMLElement,
    pct: row.querySelector('.st-pct') as HTMLElement,
    items: row.querySelector('.st-items') as HTMLElement,
    bytes: row.querySelector('.st-bytes') as HTMLElement,
    rate: row.querySelector('.st-rate') as HTMLElement,
  };
}

function onCmpEvent(ev: CmpEv) {
  if (!cmpActive || !ev.phase) return;
  if (ev.purpose && ev.purpose !== 'compare') return; // apply 的事件归子窗口
  if (ev.kind === 'phase_start') {
    for (const done of cmpRows.querySelectorAll('.stagerow.active')) {
      done.classList.remove('active');
      done.classList.add('done');
      (done.querySelector('.st-ico') as HTMLElement).textContent = '✓';
      const bar = done.querySelector('.st-bar > i') as HTMLElement | null;
      if (bar) bar.style.width = '100%';
      const pct = done.querySelector('.st-pct') as HTMLElement | null;
      if (pct && pct.textContent) pct.textContent = '100%';
    }
    const r = cmpRow(ev.phase);
    r.row.classList.add('active');
    if (ev.label) r.items.textContent = ev.label;
  } else if (ev.kind === 'progress') {
    const r = cmpRow(ev.phase);
    const idn = ev.items_done ?? 0, it = ev.items_total ?? 0;
    const bd = ev.bytes_done ?? 0, bt = ev.bytes_total ?? 0;
    r.items.textContent = it ? `${idn} / ${it} 项` : `${idn} 项`;
    r.bytes.textContent = bt ? `${humanSize(bd) || '0 B'} / ${humanSize(bt)}` : bd ? humanSize(bd) : '';
    const pctV = bt > 0 ? (bd / bt) * 100 : it > 0 ? (idn / it) * 100 : 0;
    r.pct.textContent = bt > 0 || it > 0 ? `${Math.floor(pctV)}%` : '';
    r.bar.style.width = `${pctV}%`;
    const ts = ev.ts_ms ?? Date.now();
    const prev = cmpRate.get(ev.phase);
    if (prev && ts > prev.t) {
      const inst = ((bd - prev.b) * 1000) / (ts - prev.t);
      const ema = prev.ema > 0 ? prev.ema * 0.7 + inst * 0.3 : inst;
      cmpRate.set(ev.phase, { t: ts, b: bd, ema });
      r.rate.textContent = ema > 512 * 1024 ? `${(ema / (1 << 20)).toFixed(1)} MiB/s` : '';
    } else if (!prev) {
      cmpRate.set(ev.phase, { t: ts, b: bd, ema: 0 });
    }
  }
}

cmpCancelBtn.addEventListener('click', () => {
  cmpCancelBtn.disabled = true;
  cmpCancelBtn.textContent = '取消中…（等在飞的块收尾）';
  invoke('cancel_run').catch(() => {});
  setStatus('正在取消比对…');
});

// 显示模式：树状分组 / 折叠 / 路径模式（选择均记住）
const btnPathMode = $<HTMLButtonElement>('btn-pathmode');
const btnGroup = $<HTMLButtonElement>('btn-group');
const btnFold = $<HTMLButtonElement>('btn-fold');
function renderModeButtons() {
  btnPathMode.textContent = pathMode === 'rel' ? '相对路径' : '完整路径';
  btnGroup.textContent = sort ? `排序：${sort.key}` : grouped ? '树状分组' : '平铺列表';
  btnGroup.classList.toggle('on', grouped && !sort);
  btnGroup.title = sort ? '点此清除排序，回到分组视图' : '切换：树状分组（按目录聚合，FFS 同款）↔ 平铺列表';
  btnFold.classList.toggle('hidden', !grouped || !!sort);
  btnFold.textContent = collapsedDirs.size > 0 ? '全部展开' : '全部折叠';
}

/// 点表头排序：同一键再点换方向，第三下清除回计划原序
function toggleSort(key: SortKey) {
  if (!sort || sort.key !== key) sort = { key, dir: key === 'path' || key === 'action' ? 1 : -1 };
  else if (sort.dir === (key === 'path' || key === 'action' ? 1 : -1)) sort = { key, dir: (sort.dir === 1 ? -1 : 1) as 1 | -1 };
  else sort = null;
  renderModeButtons();
  renderTable();
}
for (const el of document.querySelectorAll<HTMLElement>('#plantable th .sortable')) {
  el.addEventListener('click', () => toggleSort(el.dataset.sort as SortKey));
}
btnPathMode.addEventListener('click', () => {
  pathMode = pathMode === 'rel' ? 'full' : 'rel';
  localStorage.setItem('sd.pathmode', pathMode);
  renderModeButtons();
  renderTable();
});
btnGroup.addEventListener('click', () => {
  // 排序中点它 = 清排序回到分组，而不是在"排序 + 平铺"里再切一层
  if (sort) sort = null;
  else grouped = !grouped;
  localStorage.setItem('sd.grouped', grouped ? 'on' : 'off');
  collapsedDirs.clear();
  renderModeButtons();
  renderTable();
});
btnFold.addEventListener('click', () => {
  if (collapsedDirs.size > 0) collapsedDirs.clear();
  else for (const d of lastGroupDirs) collapsedDirs.add(d);
  renderModeButtons();
  renderTable();
});
renderModeButtons();

// ---------- P1：工具栏变体副标题 + 齿轮 + 交换 ----------

const RIGOR_HINT: Record<string, string> = {
  quick: '只比 size 与时间', fast: '抽样摘要', standard: '哈希 + 缓存', paranoid: '全量重哈希 + 复制后校验',
};
const MODE_HINT: Record<string, string> = {
  mirror: 'source 为准', sync: '双向', enrich: '只增不删',
};
const CONFLICT_HINT: Record<string, string> = {
  report: '冲突只报告', copy: '冲突留副本', newer: '新者胜',
};
const btnCmpCfg = $<HTMLButtonElement>('btn-cmpcfg');
const btnSyncCfg = $<HTMLButtonElement>('btn-synccfg');
const btnSwap = $<HTMLButtonElement>('btn-swap');

/// 按钮上直接写清楚"这一下按下去会发生什么"（FFS 的 Compare/Synchronize 副标题同款）
function renderVariants() {
  const j = currentJob;
  $('cmp-variant').textContent = j ? `${j.rigor} · ${RIGOR_HINT[j.rigor] ?? ''}` : '选择任务';
  $('sync-variant').textContent = j ? `${j.mode} · ${MODE_HINT[j.mode] ?? ''}` : '先比对';
  btnCmpCfg.disabled = !j;
  btnSyncCfg.disabled = !j;
  btnSwap.disabled = !j || busy;
  if (j) {
    btnCmpCfg.title = `比对设置：严谨级 ${j.rigor} / 大小写 / symlink`;
    btnSyncCfg.title = `同步设置：模式 ${j.mode}${j.versioning ? ' / 版本控制开' : ''}`;
    btnSwap.title = `交换：${j.source} ⇄ ${j.target}（写回任务文件）`;
  }
}
btnCmpCfg.addEventListener('click', () => { if (currentJob) openEditor(currentJob.name, '基本'); });
btnSyncCfg.addEventListener('click', () => { if (currentJob) openEditor(currentJob.name, '行为'); });

/// FFS 的 ⇄ 换的是内存里那份配置；我们的任务是磁盘上的具名 TOML，
/// 所以交换必须落盘——否则计划头里的两个根和任务文件说的不是一回事，
/// 运行日志与 archive 刷新都会指向错误的方向。
btnSwap.addEventListener('click', async () => {
  if (!currentJob || busy) return;
  const name = currentJob.name;
  const j = await invoke<JobFull>('get_job', { name }).catch((e) => { setStatus(`读取任务失败：${e}`, 'err'); return null; });
  if (!j) return;
  const warn = j.mode === 'mirror'
    ? `\n\nmirror 模式下这会调转主从：交换后以原 target 为准。`
    : '';
  if (!confirm(`交换 '${name}' 的两个根目录？\n\nsource ← ${j.target}\ntarget ← ${j.source}${warn}\n\n任务文件会被改写，当前比对结果作废。`)) return;
  const prev = { source: j.source, target: j.target };
  const swapped: JobFull = { ...j, source: j.target, target: j.source };
  try {
    await invoke('save_job', { name, job: swapped });
    jobs = await invoke<JobDto[]>('list_jobs');
    currentJob = jobs.find((x) => x.name === name) ?? currentJob;
    plan = null; checked = []; flipped = []; chips.clear();
    ovFilter = null; ovExpanded.clear();
    renderJobs();
    renderAll();
    pathEl.textContent = `${currentJob!.source}   ⇄   ${currentJob!.target}`;
    btnSync.disabled = true;
    renderVariants();
    setStatusUndo(`已交换 '${name}' 的两个根 — 重新 Compare（Ctrl+R）`, '撤销交换', async () => {
      const cur = await invoke<JobFull>('get_job', { name });
      await invoke('save_job', { name, job: { ...cur, ...prev } });
      jobs = await invoke<JobDto[]>('list_jobs');
      currentJob = jobs.find((x) => x.name === name) ?? currentJob;
      renderJobs();
      pathEl.textContent = `${currentJob!.source}   ⇄   ${currentJob!.target}`;
      renderVariants();
      setStatus(`已还原 '${name}' 的两个根`);
    });
  } catch (e) {
    setStatus(`交换失败：${e}`, 'err');
  }
});

// ---------- P1：拖放目录到路径框 ----------
//
// Tauri v2 的 dragDropEnabled 默认开着，webview 里的 HTML5 drop 事件是收不到的——
// 必须走 onDragDropEvent，且 payload.position 是**物理像素**，要自己换算成 CSS 像素。

function dropTargetAt(px: number, py: number): HTMLInputElement | null {
  if (editModal.classList.contains('hidden')) return null;
  const r = window.devicePixelRatio || 1;
  const x = px / r, y = py / r;
  for (const el of edForm.querySelectorAll<HTMLInputElement>('input[data-drop]')) {
    const b = el.getBoundingClientRect();
    if (x >= b.left && x <= b.right && y >= b.top && y <= b.bottom) return el;
  }
  return null;
}

function clearDropHint() {
  for (const el of edForm.querySelectorAll<HTMLInputElement>('input[data-drop]')) el.classList.remove('dropon');
}

async function wireDragDrop() {
  await getCurrentWebview().onDragDropEvent((ev) => {
    const p = ev.payload as unknown as { type: string; paths?: string[]; position?: { x: number; y: number } };
    if (p.type === 'leave') { clearDropHint(); return; }
    const pos = p.position;
    if (!pos) return;
    const el = dropTargetAt(pos.x, pos.y);
    if (p.type === 'over' || p.type === 'enter') {
      clearDropHint();
      if (el) el.classList.add('dropon');
      return;
    }
    if (p.type !== 'drop') return;
    clearDropHint();
    const first = p.paths?.[0];
    if (!el || !first) return;
    void (async () => {
      // 丢进来的是文件时取它所在目录——根目录字段要的是目录
      let v = first;
      try {
        const info = await invoke<PathVerdict>('inspect_paths', { source: first, target: '' });
        if (info.source.exists && !info.source.is_dir) {
          const i = Math.max(v.lastIndexOf('\\'), v.lastIndexOf('/'));
          if (i > 0) v = v.slice(0, i);
        }
      } catch { /* 拿不到就按原样填 */ }
      el.value = v;
      pushHistory(v);
      scheduleVerdict();
      setStatus(`已填入：${v}`);
    })();
  });
}

// ---------- P1：差异表右键菜单 ----------

const ctxEl = $('ctxmenu');
interface CtxItem { label: string; disabled?: boolean; danger?: boolean; sep?: boolean; run?: () => void }

function closeCtx() { ctxEl.classList.add('hidden'); }
document.addEventListener('click', closeCtx);
document.addEventListener('scroll', closeCtx, true);

function copyText(s: string) {
  navigator.clipboard?.writeText(s).then(
    () => setStatus(`已复制：${s}`),
    () => setStatus('复制失败（剪贴板不可用）', 'err'),
  );
}

/// 排除项写回任务的 exclude。扫描阶段的剪枝要下次 Compare 才生效，
/// 所以提示里必须说清楚，并留一条撤销。
async function addExclude(mask: string) {
  if (!currentJob) return;
  const name = currentJob.name;
  try {
    const j = await invoke<JobFull>('get_job', { name });
    if (j.exclude.includes(mask)) { setStatus(`任务里已有这条排除：${mask}`); return; }
    const prev = [...j.exclude];
    await invoke('save_job', { name, job: { ...j, exclude: [...prev, mask] } });
    jobs = await invoke<JobDto[]>('list_jobs');
    currentJob = jobs.find((x) => x.name === name) ?? currentJob;
    renderJobs();
    setStatusUndo(`已加入 exclude：${mask} — 重新 Compare（Ctrl+R）后生效`, '撤销排除', async () => {
      const cur = await invoke<JobFull>('get_job', { name });
      await invoke('save_job', { name, job: { ...cur, exclude: prev } });
      jobs = await invoke<JobDto[]>('list_jobs');
      currentJob = jobs.find((x) => x.name === name) ?? currentJob;
      renderJobs();
      setStatus(`已撤销排除：${mask}`);
    });
  } catch (e) {
    setStatus(`写入 exclude 失败：${e}`, 'err');
  }
}

function openCtx(x: number, y: number, items: CtxItem[]) {
  ctxEl.innerHTML = '';
  for (const it of items) {
    if (it.sep) {
      const d = document.createElement('div');
      d.className = 'ctxsep';
      ctxEl.appendChild(d);
      continue;
    }
    const d = document.createElement('div');
    d.className = 'ctxitem' + (it.disabled ? ' off' : '') + (it.danger ? ' danger' : '');
    d.textContent = it.label;
    if (!it.disabled && it.run) {
      d.addEventListener('click', (e) => { e.stopPropagation(); closeCtx(); it.run!(); });
    }
    ctxEl.appendChild(d);
  }
  // 定位走 CSSOM：style="" 属性会被注入 nonce 后的 CSP 拦掉
  ctxEl.classList.remove('hidden');
  const w = ctxEl.offsetWidth, h = ctxEl.offsetHeight;
  // 下界 6：窗口比菜单还窄时 min() 会算出负数，菜单整个跑到屏幕外
  ctxEl.style.left = `${Math.max(6, Math.min(x, window.innerWidth - w - 6))}px`;
  ctxEl.style.top = `${Math.max(6, Math.min(y, window.innerHeight - h - 6))}px`;
}

function rowMenu(i: number, x: number, y: number) {
  const p = plan!;
  const op = eff(i);
  const [sp, tp] = sidePaths(op);
  const sAbs = sp ? fullPath(p.header.source_root, sp) : null;
  const tAbs = tp ? fullPath(p.header.target_root, tp) : null;
  const rel = op.path;
  const base = baseOf(rel);
  const dot = base.lastIndexOf('.');
  const ext = dot > 0 ? base.slice(dot + 1) : '';
  const dir = dirOf(rel);
  const canFlip = !!p.reversed[i] && selectable(p.ops[i]);
  const sameDir = visibleIdx().filter((k) => dirOf(eff(k).path) === dir && selectable(eff(k)));
  const items: CtxItem[] = [
    { label: '在资源管理器中显示 · source', disabled: !sAbs, run: () => { invoke('reveal', { path: sAbs }).catch((e) => setStatus(String(e), 'err')); } },
    { label: '在资源管理器中显示 · target', disabled: !tAbs, run: () => { invoke('reveal', { path: tAbs }).catch((e) => setStatus(String(e), 'err')); } },
    { sep: true, label: '' },
    { label: '复制完整路径', run: () => copyText((sAbs ?? tAbs)!) },
    { label: '复制相对路径', run: () => copyText(rel) },
    { sep: true, label: '' },
    { label: ext ? `排除此类型 */*.${ext}` : '排除此类型（无扩展名）', disabled: !ext || !currentJob, run: () => addExclude(`*/*.${ext}`) },
    { label: dir ? `排除此目录 /${dir}/` : '排除此目录（已在根下）', disabled: !dir || !currentJob, run: () => addExclude(`/${dir}/`) },
    { sep: true, label: '' },
    { label: flipped[i] ? '恢复原方向' : '反向此行', disabled: !canFlip, run: () => { flipped[i] = !flipped[i]; renderAll(); } },
    { label: '只勾选此项', run: () => { checked = checked.map((_, k) => k === i && selectable(eff(k))); renderTable(); } },
    { label: `取消勾选本目录（${sameDir.length}）`, disabled: sameDir.length === 0, run: () => { for (const k of sameDir) checked[k] = false; renderTable(); } },
  ];
  openCtx(x, y, items);
}

// ---------- P3：漏斗（作用于当前结果的视图过滤） ----------
//
// 语义与 FFS 一致：**视图即动作集**——被漏斗隐藏的行不会被执行。
// 这条在确认单里会明写，因为它改变了"勾了就一定跑"的旧直觉。

const funnelPop = $('funnelpop');
const btnFunnel = $<HTMLButtonElement>('btn-funnel');
const fpMasks = $<HTMLTextAreaElement>('fp-masks');
const fpMin = $<HTMLInputElement>('fp-min');
const fpMax = $<HTMLInputElement>('fp-max');
let maskTimer: number | null = null;

function renderFunnelBtn() {
  const n = funnelActive();
  btnFunnel.textContent = n ? `🔻 筛选 ${n}` : '🔻 筛选';
  btnFunnel.classList.toggle('on', n > 0);
}

/// 掩码判定走 Rust（去抖 200ms）：前端绝不自己写一份 glob，
/// 否则界面里试通的掩码写进 exclude 后行为可能不一样。
async function recomputeMasks() {
  if (!plan) { maskHit = []; return; }
  if (vfilter.masks.length === 0) {
    maskHit = new Array(plan.ops.length).fill(false);
    return;
  }
  try {
    maskHit = await invoke<boolean[]>('mask_match', {
      masks: vfilter.masks,
      paths: plan.ops.map((_, i) => eff(i).path),
    });
  } catch (e) {
    maskHit = new Array(plan.ops.length).fill(false);
    setStatus(`掩码匹配失败：${e}`, 'err');
  }
}

function scheduleMasks() {
  if (maskTimer !== null) clearTimeout(maskTimer);
  maskTimer = window.setTimeout(async () => {
    vfilter.masks = fpMasks.value.split('\n').map((s) => s.trim()).filter(Boolean);
    await recomputeMasks();
    renderFunnelBtn();
    renderAll();
    renderFunnelStat();
  }, 200);
}

function readSizeInputs() {
  const num = (el: HTMLInputElement) => (el.value.trim() === '' ? null : Math.max(0, Number(el.value)));
  vfilter.minMB = num(fpMin);
  vfilter.maxMB = num(fpMax);
  renderFunnelBtn();
  renderAll();
  renderFunnelStat();
}

function renderFunnelStat() {
  const el = document.getElementById('fp-stat');
  if (!el) return;
  if (!plan) { el.textContent = ''; return; }
  const shown = visibleIdx().length;
  const hid = plan.ops.length - shown;
  el.textContent = hid > 0
    ? `隐藏 ${hid} 项 —— 这些行不会被执行（显示 ${shown} / ${plan.ops.length}）`
    : `没有行被隐藏（共 ${plan.ops.length} 项）`;
}

btnFunnel.addEventListener('click', (e) => {
  e.stopPropagation();
  if (!funnelPop.classList.contains('hidden')) { funnelPop.classList.add('hidden'); return; }
  fpMasks.value = vfilter.masks.join('\n');
  fpMin.value = vfilter.minMB === null ? '' : String(vfilter.minMB);
  fpMax.value = vfilter.maxMB === null ? '' : String(vfilter.maxMB);
  funnelPop.classList.remove('hidden');
  // 定位走 CSSOM（CSP nonce 下 style="" 会被拦）
  const b = btnFunnel.getBoundingClientRect();
  const w = funnelPop.offsetWidth;
  funnelPop.style.left = `${Math.max(6, Math.min(b.left, window.innerWidth - w - 6))}px`;
  funnelPop.style.top = `${b.bottom + 4}px`;
  renderFunnelStat();
  fpMasks.focus();
});
funnelPop.addEventListener('click', (e) => e.stopPropagation());
document.addEventListener('click', () => funnelPop.classList.add('hidden'));
fpMasks.addEventListener('input', scheduleMasks);
fpMin.addEventListener('input', readSizeInputs);
fpMax.addEventListener('input', readSizeInputs);
for (const b of document.querySelectorAll<HTMLButtonElement>('#fp-days .chip')) {
  b.addEventListener('click', () => {
    for (const o of document.querySelectorAll('#fp-days .chip')) o.classList.remove('on');
    b.classList.add('on');
    vfilter.days = b.dataset.days ? Number(b.dataset.days) : null;
    renderFunnelBtn();
    renderAll();
    renderFunnelStat();
  });
}
$('fp-clear').addEventListener('click', async () => {
  vfilter = { masks: [], minMB: null, maxMB: null, days: null };
  fpMasks.value = ''; fpMin.value = ''; fpMax.value = '';
  for (const o of document.querySelectorAll('#fp-days .chip')) o.classList.remove('on');
  document.querySelector('#fp-days .chip')?.classList.add('on');
  await recomputeMasks();
  renderFunnelBtn();
  renderAll();
  renderFunnelStat();
});
$('fp-done').addEventListener('click', () => funnelPop.classList.add('hidden'));
/// 临时掩码升格为任务的持久 exclude：同一套语法，从"这一次"变成"每一次"
$('fp-promote').addEventListener('click', async () => {
  if (!currentJob) { setStatus('先选一个任务', 'err'); return; }
  const masks = fpMasks.value.split('\n').map((s) => s.trim()).filter(Boolean);
  if (!masks.length) { setStatus('先写至少一条掩码', 'err'); return; }
  const name = currentJob.name;
  try {
    const j = await invoke<JobFull>('get_job', { name });
    const prev = [...j.exclude];
    const add = masks.filter((m) => !prev.includes(m));
    if (!add.length) { setStatus('这些掩码任务里都已经有了'); return; }
    await invoke('save_job', { name, job: { ...j, exclude: [...prev, ...add] } });
    jobs = await invoke<JobDto[]>('list_jobs');
    currentJob = jobs.find((x) => x.name === name) ?? currentJob;
    renderJobs();
    funnelPop.classList.add('hidden');
    setStatusUndo(
      `已写进 '${name}' 的 exclude：${add.join('、')} — 下次 Compare 起在扫描阶段就剪枝`,
      '撤销',
      async () => {
        const cur = await invoke<JobFull>('get_job', { name });
        await invoke('save_job', { name, job: { ...cur, exclude: prev } });
        jobs = await invoke<JobDto[]>('list_jobs');
        currentJob = jobs.find((x) => x.name === name) ?? currentJob;
        renderJobs();
        setStatus('已撤销');
      },
    );
  } catch (e) {
    setStatus(`写入失败：${e}`, 'err');
  }
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
  if (!ctxEl.classList.contains('hidden') && e.key === 'Escape') { closeCtx(); return; }
  if (!editModal.classList.contains('hidden')) {
    if (e.key === 'Escape') editModal.classList.add('hidden');
    return;
  }
  if (!logModal.classList.contains('hidden')) {
    if (e.key === 'Escape') logModal.classList.add('hidden');
    return;
  }
  if (!modalEl.classList.contains('hidden')) {
    if (e.key === 'Escape') modalEl.classList.add('hidden');
    if (e.key === 'Enter' && !modalOk.disabled) doSync();
    return;
  }
  // F5 / F9 = FFS 的比对 / 同步；Ctrl+R / Enter 依然管用
  if (e.key === 'F5') { e.preventDefault(); doCompare(); }
  else if (e.key === 'F9') { e.preventDefault(); if (plan && !busy && !btnSync.disabled) openConfirm(); }
  else if (mod && e.key.toLowerCase() === 'r') { e.preventDefault(); doCompare(); }
  else if (mod && e.key.toLowerCase() === 'f') { e.preventDefault(); searchEl.focus(); }
  else if (e.key === 'Enter' && document.activeElement !== searchEl && plan && !busy && !btnSync.disabled) openConfirm();
});

// ---------- 初始化 ----------

(async function init() {
  if (navigator.userAgent.includes('Macintosh')) document.body.classList.add('mac');
  renderVariants();
  wireDragDrop().catch(() => { /* 拖放不可用不影响手打路径 */ });
  await listen<CmpEv>('run-progress', (ev) => onCmpEvent(ev.payload));
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
    refreshLastSyncs();
    $('jobsdir').textContent = await invoke<string>('jobs_dir');
    try { $('appver').textContent = 'v' + (await getVersion()); } catch { /* 权限缺省时忽略 */ }
    setStatus(jobs.length ? '选择左侧任务开始' : '没有任务 — 在 jobs 目录放 <名字>.toml');
  } catch (e) {
    setStatus(`初始化失败：${e}`, 'err');
  }
})();
