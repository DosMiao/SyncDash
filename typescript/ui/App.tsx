// SyncDash main window.
//
// This component owns the session state (selected job, compare result, check/flip vectors, view filters)
// and every action that crosses the Tauri boundary; everything under components/ is presentation fed by
// props. Sync semantics stay on the Rust side — reverse ops are precomputed there (reversed[i]), so the
// frontend carries zero of them and cannot drift.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { CircleCheck, FolderSearch } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';
import { getCurrentWebview } from '@tauri-apps/api/webview';

import * as ipc from '../core/ipc';
import { EMPTY_FILTER, computeVisible, finalIdx, funnelActive } from '../core/filter';
import { addExcludeEntries } from '../core/junk';
import { baseOf, dirOf, fullPath, p2 } from '../core/format';
import { canFlip, eff, metaOf, selectable, sidePaths } from '../core/plan';
import type { Chip, PlanDto, Sort, SortKey } from '../core/plan';
import type { ViewFilter } from '../core/filter';
import type { RowSpec } from './hooks/useVirtualRows';
import type { JobDto } from '../core/types/generated/JobDto';
import type { LegacyProgress } from '../core/types/generated/LegacyProgress';
import type { PreflightDto } from '../core/types/generated/PreflightDto';
import type { RunRecord } from '../core/types/generated/RunRecord';

import { useStatus } from './hooks/useStatus';
import { useZoomControl } from './hooks/useZoomControl';
import { ComparePanel } from './components/ComparePanel';
import { ConfirmSheet } from './components/ConfirmSheet';
import { FilterBar } from './components/FilterBar';
import { FunnelPopover } from './components/FunnelPopover';
import { JobEditor } from './components/JobEditor';
import { LogPanel } from './components/LogPanel';
import { Overview } from './components/Overview';
import { PathLine } from './components/PathLine';
import { PlanTable } from './components/PlanTable';
import { SamePanel } from './components/SamePanel';
import { SettingsSheet } from './components/SettingsSheet';
import { Sidebar } from './components/Sidebar';
import { StatusBar } from './components/StatusBar';
import { Toolbar } from './components/Toolbar';
import { ConfirmDialog, ContextMenu, MenuDivider, MenuItem, Placeholder } from './components/ui';
import type { CmpStage } from './components/ComparePanel';
import type { ConfirmTotals } from './components/ConfirmSheet';
import type { EditorApi } from './components/JobEditor';

const HIST_KEY = 'sd.pathhist';

/// One entry in a row's right-click menu. Built at open time so each closure sees the row and the
/// plan as they were when you right-clicked — a menu is transient, and a stale entry would be worse
/// than a frozen one.
interface CtxItem {
  label: string;
  disabled?: boolean;
  danger?: boolean;
  sep?: boolean;
  run?: () => void;
}

interface CtxState { x: number; y: number; items: CtxItem[] }

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

function readHistory(): string[] {
  try { return JSON.parse(localStorage.getItem(HIST_KEY) ?? '[]') as string[]; } catch { return []; }
}

export function App() {
  // Session
  const [jobs, setJobs] = useState<JobDto[]>([]);
  const [currentJob, setCurrentJob] = useState<JobDto | null>(null);
  const [cfgJob, setCfgJob] = useState<ipc.JobFull | null>(null);
  const [lastMap, setLastMap] = useState<Record<string, RunRecord>>({});
  const [appVersion, setAppVersion] = useState('');
  const [jobsDir, setJobsDir] = useState('');
  const [pathHistory, setPathHistory] = useState<string[]>(readHistory);
  /// 1:N: index of the target currently being operated on (resets when the job changes)
  const [selTarget, setSelTarget] = useState(0);

  // Compare result
  const [plan, setPlan] = useState<PlanDto | null>(null);
  const [checked, setChecked] = useState<boolean[]>([]);
  const [flipped, setFlipped] = useState<boolean[]>([]);
  const [maskHit, setMaskHit] = useState<boolean[]>([]);
  const [busy, setBusy] = useState(false);
  /// The user ticked "I confirm this is correct" in the confirm sheet (same as CLI --i-know); reset on every new compare
  const [acknowledged, setAcknowledged] = useState(false);

  // View
  const [chips, setChips] = useState<Set<Chip>>(new Set());
  const [search, setSearch] = useState('');
  const [ovFilter, setOvFilter] = useState<string | null>(null);
  const [ovExpanded, setOvExpanded] = useState<Set<string>>(new Set());
  const [ovCollapsed, setOvCollapsed] = useState(() => localStorage.getItem('sd.ov') !== 'open');
  const [sort, setSort] = useState<Sort | null>(null);
  const [vfilter, setVfilter] = useState<ViewFilter>(EMPTY_FILTER);
  const [pathMode, setPathMode] = useState<'rel' | 'full'>(() => (localStorage.getItem('sd.pathmode') === 'full' ? 'full' : 'rel'));
  const [grouped, setGrouped] = useState(() => localStorage.getItem('sd.grouped') !== 'off');
  const [collapsedDirs, setCollapsedDirs] = useState<Set<string>>(new Set());

  // Panels and overlays
  const [editor, setEditor] = useState<{ name: string | null; focusGroup?: string } | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [preflight, setPreflight] = useState<PreflightDto | null>(null);
  const [preflightError, setPreflightError] = useState<string | null>(null);
  const [funnelAnchor, setFunnelAnchor] = useState<DOMRect | null>(null);
  const [ctx, setCtx] = useState<CtxState | null>(null);
  const [logOpen, setLogOpen] = useState(false);
  const [logReload, setLogReload] = useState(0);
  const [sameOpen, setSameOpen] = useState(false);
  const [dropOn, setDropOn] = useState<string | null>(null);
  /// The diff table's scroll container. App renders it, so App owns it and hands it down.
  const [tableWrap, setTableWrap] = useState<HTMLDivElement | null>(null);
  /// Pending root swap, held with the job it read so the confirmation can spell out both roots
  const [askSwap, setAskSwap] = useState<{ name: string; job: ipc.JobFull } | null>(null);
  /// The two regions holding droppable path fields. A ref rather than state: the drag handler is
  /// registered once and reads this at drop time, so state here would only hand it a stale closure.
  const dropScope = useRef<{ editor: HTMLElement | null; path: HTMLElement | null }>({ editor: null, path: null });

  // Compare progress (fed by the run-progress stream)
  const [cmpActive, setCmpActive] = useState(false);
  const [cmpStages, setCmpStages] = useState<CmpStage[]>([]);
  const [cmpCancelling, setCmpCancelling] = useState(false);
  /// Rate EMA (0.7 old + 0.3 new): the instantaneous rate swings wildly with file size
  const cmpRate = useRef(new Map<string, { t: number; b: number; ema: number }>());

  // Watch
  const [watchSecs, setWatchSecs] = useState<number | null>(null);
  const watchNext = useRef(0);

  const editorApi = useRef<EditorApi | null>(null);
  const { status, set: setStatus, withUndo: setStatusUndo, runUndo } = useStatus('');
  const zoom = useZoomControl();

  // Derived view

  const visible = useMemo(() => (
    plan ? computeVisible({ plan, flipped, chips, search, ovFilter, vfilter, maskHit, sort }) : []
  ), [plan, flipped, chips, search, ovFilter, vfilter, maskHit, sort]);

  const final = useMemo(() => finalIdx(visible, checked), [visible, checked]);

  const { rowPlan, groupDirs } = useMemo(() => {
    if (!plan) return { rowPlan: [] as RowSpec[], groupDirs: [] as string[] };
    // Always flat while sorting: grouping relies on the invariant that same-directory rows are
    // consecutive in the plan, and sorting destroys it
    if (!grouped || sort) {
      return { rowPlan: visible.map((i) => ({ kind: 'row', i, groupDir: null }) as RowSpec), groupDirs: [] };
    }
    const groups: { dir: string; items: number[] }[] = [];
    for (const i of visible) {
      const d = dirOf(eff(plan, flipped, i).path);
      if (!groups.length || groups[groups.length - 1].dir !== d) groups.push({ dir: d, items: [] });
      groups[groups.length - 1].items.push(i);
    }
    const out: RowSpec[] = [];
    for (const g of groups) {
      out.push({ kind: 'grp', dir: g.dir, items: g.items });
      if (!collapsedDirs.has(g.dir)) for (const i of g.items) out.push({ kind: 'row', i, groupDir: g.dir });
    }
    return { rowPlan: out, groupDirs: [...new Set(groups.map((g) => g.dir))] };
  }, [plan, visible, flipped, grouped, sort, collapsedDirs]);

  /// The stats bar counts exactly what will run (checked ∩ visible), matching the confirm sheet
  const stats = useMemo(() => {
    if (!plan) return null;
    const s = { copy: 0, upd: 0, mv: 0, del: 0, conflicts: plan.header.conflict_count, bytes: 0, flips: 0 };
    for (const i of final) {
      const op = eff(plan, flipped, i);
      switch (op.action) {
        case 'copy': s.copy++; s.bytes += op.size ?? 0; break;
        case 'update': case 'chmod': s.upd++; s.bytes += op.size ?? 0; break;
        case 'move': s.mv++; break;
        case 'delete': case 'delete_dir': s.del++; break;
      }
    }
    s.flips = flipped.filter(Boolean).length;
    return s;
  }, [plan, final, flipped]);

  // Helpers

  const pushHistory = useCallback((p: string) => {
    const v = p.trim();
    if (!v) return;
    setPathHistory((prev) => {
      const list = [v, ...prev.filter((x) => x.toLowerCase() !== v.toLowerCase())].slice(0, 12);
      localStorage.setItem(HIST_KEY, JSON.stringify(list));
      return list;
    });
  }, []);

  const refreshJobs = useCallback(async (keepName?: string) => {
    const list = await ipc.listJobs();
    setJobs(list);
    if (keepName) setCurrentJob((cur) => list.find((x) => x.name === keepName) ?? cur);
    return list;
  }, []);

  const refreshLastSyncs = useCallback(() => {
    ipc.lastSyncs().then(setLastMap).catch(() => { /* missing logs are not fatal */ });
  }, []);

  /// A new plan means a new row set, so the mask cache is stale; the funnel criteria themselves persist
  /// (watching the same files for several rounds is common)
  const recomputeMasks = useCallback(async (p: PlanDto | null, f: boolean[], masks: string[]) => {
    if (!p) { setMaskHit([]); return; }
    if (masks.length === 0) { setMaskHit([]); return; }
    try {
      setMaskHit(await ipc.maskMatch(masks, p.ops.map((_, i) => eff(p, f, i).path)));
    } catch (e) {
      setMaskHit([]);
      setStatus(`Mask matching failed: ${e}`, 'err');
    }
  }, [setStatus]);

  // Actions

  const doCompare = useCallback(async () => {
    if (!currentJob || busy) return;
    setSameOpen(false); // a new compare = a new snapshot, so the old identical-items list is stale
    setBusy(true);
    setStatus(`Comparing '${currentJob.name}' ...`);
    setCmpStages([]);
    cmpRate.current.clear();
    setCmpCancelling(false);
    setCmpActive(true);
    try {
      setAcknowledged(false);
      const p = await ipc.compareJob(currentJob.name, selTarget);
      const f = p.ops.map(() => false);
      setPlan(p);
      setChecked(p.ops.map((op) => selectable(op)));
      setFlipped(f);
      setChips(new Set());
      setOvFilter(null);
      setOvExpanded(new Set());
      setSort(null);
      await recomputeMasks(p, f, vfilter.masks);
      // The counts bar to the right already says what was scanned and what is showing, and the
      // placeholder in the table says "identical" in full — this line only has to say the result
      setStatus(
        p.ops.length === 0
          ? 'Both sides identical'
          : `${p.ops.length} items · ${p.header.conflict_count} conflicts`,
        p.header.conflict_count > 0 ? 'err' : '',
      );
    } catch (e) {
      setPlan(null);
      setChecked([]);
      setFlipped([]);
      const cancelled = String(e) === 'cancelled';
      setStatus(cancelled ? 'Compare cancelled' : `Compare failed: ${e}`, cancelled ? '' : 'err');
    }
    setCmpActive(false);
    setBusy(false);
  }, [currentJob, busy, selTarget, vfilter.masks, recomputeMasks, setStatus]);

  const openConfirm = useCallback(async () => {
    if (!currentJob || !plan || busy) return;
    const hiddenChecked = checked.filter(Boolean).length - final.length;
    if (final.length === 0) {
      setStatus(
        hiddenChecked > 0 ? 'Every checked row is hidden by a filter — clear the filter first' : 'Nothing is checked',
        'err',
      );
      return;
    }
    setPreflight(null);
    setPreflightError(null);
    setConfirmOpen(true);
    try {
      setPreflight(await ipc.preflight(currentJob.name, plan, final.map((i) => eff(plan, flipped, i)), acknowledged, selTarget));
    } catch (e) {
      setPreflightError(String(e));
    }
  }, [currentJob, plan, busy, checked, final, flipped, acknowledged, selTarget, setStatus]);

  const doSync = useCallback(async () => {
    setConfirmOpen(false);
    if (!currentJob || !plan || busy) return;
    const ops = final.map((i) => eff(plan, flipped, i));
    setBusy(true);
    setStatus(`Synchronizing '${currentJob.name}' (${ops.length} items)...`);
    // Whether the progress window stays during a sync is its own Auto-close / When-finished business
    ipc.openProgressWindow().catch(() => {});
    try {
      const r = await ipc.applyJob(currentJob.name, plan, ops, acknowledged, selTarget);
      setStatus(
        r.cancelled
          ? `Stopped: cancelled after ${r.done} run — re-checking...`
          : `Done: ${r.done} run, ${r.skipped} skipped, ${r.errors} errors — re-checking...`,
        r.errors ? 'err' : 'ok',
      );
      setBusy(false);
      refreshLastSyncs();
      setLogReload((k) => k + 1);
      await doCompare();
    } catch (e) {
      setStatus(`Synchronize failed: ${e}`, 'err');
      setBusy(false);
    }
  }, [currentJob, plan, busy, final, flipped, acknowledged, selTarget, doCompare, refreshLastSyncs, setStatus]);

  const selectJob = useCallback((j: JobDto) => {
    setCurrentJob(j);
    setPlan(null);
    setChecked([]);
    setFlipped([]);
    setChips(new Set());
    setSearch('');
    setOvFilter(null);
    setOvExpanded(new Set());
    setSort(null);
    setVfilter(EMPTY_FILTER);
    setMaskHit([]);
    setSelTarget(0);
    setSameOpen(false);
    setWatchSecs(null);
    setStatus(`${j.name} · ${j.mode}${j.rigor !== 'standard' ? ` · ${j.rigor}` : ''}`);
  }, [setStatus]);

  /// Write a root edited on the main screen back to the job TOML. Changing a root invalidates the current
  /// plan, so clear it too. For multi-target jobs, only the currently selected target changes.
  const saveRoot = useCallback(async (which: 'source' | 'target', value: string) => {
    if (!currentJob) return;
    const name = currentJob.name;
    const v = value.trim();
    if (!v) return;
    try {
      const j = await ipc.getJob(name);
      const ts = j.targets && j.targets.length ? [...j.targets] : [j.target];
      const before = which === 'source' ? j.source : ts[selTarget];
      if (before === v) return; // unchanged means no disk write — otherwise every blur would bump the mtime
      const next: ipc.JobFull = which === 'source'
        ? { ...j, source: v }
        : { ...j, target: selTarget === 0 ? v : j.target, targets: ts.map((t, i) => (i === selTarget ? v : t)) };
      await ipc.saveJob(name, next);
      pushHistory(v);
      await refreshJobs(name);
      setPlan(null);
      setChecked([]);
      setFlipped([]);
      setStatusUndo(`Changed ${which} → ${v} — Compare again (Ctrl+R)`, 'Undo', async () => {
        const cur = await ipc.getJob(name);
        const back: ipc.JobFull = which === 'source'
          ? { ...cur, source: before }
          : { ...cur, target: selTarget === 0 ? before : cur.target, targets: (cur.targets ?? []).map((t, i) => (i === selTarget ? before : t)) };
        await ipc.saveJob(name, back);
        await refreshJobs(name);
        setStatus(`Restored ${which}`);
      });
    } catch (e) {
      setStatus(`Failed to write back to the job: ${e}`, 'err');
    }
  }, [currentJob, selTarget, pushHistory, refreshJobs, setStatus, setStatusUndo]);

  /// The FFS ⇄ swaps the in-memory config; our jobs are named TOML files on disk, so a swap has to hit
  /// the disk — otherwise the two roots in the plan header say something different from the job file, and
  /// both run logs and archive refresh point in the wrong direction. Read the job first so the
  /// confirmation can name the two roots it is about to exchange.
  const requestSwap = useCallback(async () => {
    if (!currentJob || busy) return;
    try {
      setAskSwap({ name: currentJob.name, job: await ipc.getJob(currentJob.name) });
    } catch (e) {
      setStatus(`Failed to read job: ${e}`, 'err');
    }
  }, [currentJob, busy, setStatus]);

  const doSwap = useCallback(async (name: string, j: ipc.JobFull) => {
    const prev = { source: j.source, target: j.target };
    try {
      await ipc.saveJob(name, { ...j, source: j.target, target: j.source });
      await refreshJobs(name);
      setPlan(null);
      setChecked([]);
      setFlipped([]);
      setChips(new Set());
      setOvFilter(null);
      setOvExpanded(new Set());
      setStatusUndo(`Swapped the two roots of '${name}' — Compare again (Ctrl+R)`, 'Undo swap', async () => {
        const cur = await ipc.getJob(name);
        await ipc.saveJob(name, { ...cur, ...prev });
        await refreshJobs(name);
        setStatus(`Restored the two roots of '${name}'`);
      });
    } catch (e) {
      setStatus(`Swap failed: ${e}`, 'err');
    }
  }, [refreshJobs, setStatus, setStatusUndo]);

  /// Write an exclude back into the job's exclude list. Pruning during the scan only takes effect at the
  /// next Compare, so the message has to say so and leave an undo behind.
  const addExcludes = useCallback(async (masks: string[], label: string) => {
    if (!currentJob) { setStatus('Select a job first', 'err'); return; }
    const name = currentJob.name;
    try {
      const j = await ipc.getJob(name);
      const prev = [...j.exclude];
      // Folded the way the engine folds, not by string equality: a mask typed with backslashes, in a
      // different case, or in NFD is the same rule to the filter, and appending it again would leave
      // two lines that mean one thing — and a preset box unticked next to its own pattern
      const { next, added: add } = addExcludeEntries(prev, masks);
      if (!add.length) { setStatus(`The job already has ${masks.length > 1 ? 'all of these masks' : 'this exclude'}`); return; }
      await ipc.saveJob(name, { ...j, exclude: next });
      await refreshJobs(name);
      setStatusUndo(`${label}: ${add.join(', ')} — pruned during the scan from the next Compare on`, 'Undo exclude', async () => {
        const cur = await ipc.getJob(name);
        await ipc.saveJob(name, { ...cur, exclude: prev });
        await refreshJobs(name);
        setStatus('Exclude undone');
      });
    } catch (e) {
      setStatus(`Failed to write exclude: ${e}`, 'err');
    }
  }, [currentJob, refreshJobs, setStatus, setStatusUndo]);

  /// Export the current view (the visible set, with check state). Escaping and the BOM are handled once
  /// on the Rust side.
  const exportCsv = useCallback(async () => {
    if (!plan || !currentJob) { setStatus('Compare first', 'err'); return; }
    if (!visible.length) { setStatus('The current view is empty', 'err'); return; }
    const stamp = new Date();
    const def = `${currentJob.name}-${stamp.getFullYear()}${p2(stamp.getMonth() + 1)}${p2(stamp.getDate())}.csv`;
    try {
      const path = await ipc.pickPath({ save: true, title: 'Export CSV', defaultPath: def });
      if (!path) return;
      const n = await ipc.exportCsv(
        path, plan.header,
        visible.map((i) => eff(plan, flipped, i)),
        visible.map((i) => metaOf(plan, i)),
        visible.map((i) => checked[i]),
      );
      setStatusUndo(`Exported ${n} rows to ${path}`, 'Open containing folder', () => ipc.reveal(path));
    } catch (e) {
      setStatus(`Export failed: ${e}`, 'err');
    }
  }, [plan, currentJob, visible, flipped, checked, setStatus, setStatusUndo]);

  const browseRoot = useCallback(async (which: 'source' | 'target') => {
    try {
      const p = await ipc.pickPath({
        directory: true,
        title: `Select the ${which} directory`,
        defaultPath: which === 'source' ? currentJob?.source : currentJob?.target,
      });
      if (p) await saveRoot(which, p);
    } catch (e) {
      setStatus(`Can't open the picker: ${e}`, 'err');
    }
  }, [currentJob, saveRoot, setStatus]);

  // Row interactions

  const toggleRow = useCallback((i: number, v: boolean) => {
    setChecked((prev) => { const next = [...prev]; next[i] = v; return next; });
  }, []);

  const toggleMany = useCallback((items: number[], v: boolean) => {
    setChecked((prev) => { const next = [...prev]; for (const i of items) next[i] = v; return next; });
  }, []);

  const flipRow = useCallback((i: number) => {
    setFlipped((prev) => { const next = [...prev]; next[i] = !next[i]; return next; });
  }, []);

  const foldDir = useCallback((dir: string) => {
    setCollapsedDirs((prev) => {
      const next = new Set(prev);
      if (next.has(dir)) next.delete(dir); else next.add(dir);
      return next;
    });
  }, []);

  /// Click a header to sort: clicking the same key again flips the direction, a third click clears back
  /// to the plan's order
  const toggleSort = useCallback((key: SortKey) => {
    setSort((cur) => {
      const natural: 1 | -1 = key === 'path' || key === 'action' ? 1 : -1;
      if (!cur || cur.key !== key) return { key, dir: natural };
      if (cur.dir === natural) return { key, dir: (cur.dir === 1 ? -1 : 1) as 1 | -1 };
      return null;
    });
  }, []);

  const rowMenu = useCallback((i: number, x: number, y: number) => {
    if (!plan) return;
    const op = eff(plan, flipped, i);
    const [sp, tp] = sidePaths(op);
    const sAbs = sp ? fullPath(plan.header.source_root, sp) : null;
    const tAbs = tp ? fullPath(plan.header.target_root, tp) : null;
    const rel = op.path;
    const base = baseOf(rel);
    const dot = base.lastIndexOf('.');
    const ext = dot > 0 ? base.slice(dot + 1) : '';
    const dir = dirOf(rel);
    const sameDir = visible.filter((k) => dirOf(eff(plan, flipped, k).path) === dir && selectable(eff(plan, flipped, k)));
    const copy = (s: string) => navigator.clipboard?.writeText(s).then(
      () => setStatus(`Copied: ${s}`),
      () => setStatus('Copy failed (clipboard unavailable)', 'err'),
    );
    setCtx({
      x, y,
      items: [
        { label: 'Show in Explorer · source', disabled: !sAbs, run: () => { ipc.reveal(sAbs!).catch((e) => setStatus(String(e), 'err')); } },
        { label: 'Show in Explorer · target', disabled: !tAbs, run: () => { ipc.reveal(tAbs!).catch((e) => setStatus(String(e), 'err')); } },
        { sep: true, label: '' },
        { label: 'Copy full path', run: () => copy((sAbs ?? tAbs)!) },
        { label: 'Copy relative path', run: () => copy(rel) },
        { sep: true, label: '' },
        { label: ext ? `Exclude this type */*.${ext}` : 'Exclude this type (no extension)', disabled: !ext || !currentJob, run: () => addExcludes([`*/*.${ext}`], 'Added to exclude') },
        { label: dir ? `Exclude this directory /${dir}/` : 'Exclude this directory (already at the root)', disabled: !dir || !currentJob, run: () => addExcludes([`/${dir}/`], 'Added to exclude') },
        { sep: true, label: '' },
        { label: flipped[i] ? 'Restore original direction' : 'Reverse this row', disabled: !canFlip(plan, i), run: () => flipRow(i) },
        { label: 'Check only this item', run: () => setChecked(plan.ops.map((_, k) => k === i && selectable(eff(plan, flipped, k)))) },
        { label: `Uncheck this directory (${sameDir.length})`, disabled: sameDir.length === 0, run: () => toggleMany(sameDir, false) },
      ],
    });
  }, [plan, flipped, visible, currentJob, addExcludes, flipRow, toggleMany, setStatus]);

  // Init

  useEffect(() => {
    if (navigator.userAgent.includes('Macintosh')) document.body.classList.add('mac');
    (async () => {
      try {
        const list = await refreshJobs();
        refreshLastSyncs();
        setJobsDir(await ipc.jobsDir());
        try { setAppVersion('v' + (await getVersion())); } catch { /* ignore when the permission isn't granted */ }
        setStatus(list.length ? 'Select a job on the left to start' : 'No jobs — drop a <name>.toml into the jobs directory');
      } catch (e) {
        setStatus(`Init failed: ${e}`, 'err');
      }
    })();
  }, []);

  // Progress event streams
  useEffect(() => {
    const un = listen<LegacyProgress>('progress', (ev) => {
      const { phase, detail, pct, rate } = ev.payload;
      const map: Record<string, string> = {
        'scan-source': 'Scanning source: ', 'scan-target': 'Scanning target: ', 'comparing': 'Comparing: ', 'warning': 'Warning: ',
      };
      const suffix = pct >= 0 ? `  ${pct}%${rate > 0 ? `  ${rate.toFixed(1)} MiB/s` : ''}` : '';
      setStatus((map[phase] ?? phase) + detail + suffix, phase === 'warning' ? 'err' : '');
    });
    return () => { un.then((f) => f()); };
  }, [setStatus]);

  useEffect(() => {
    const un = listen<CmpEv>('run-progress', (ev) => {
      const e = ev.payload;
      if (!e.phase) return;
      if (e.purpose && e.purpose !== 'compare') return; // apply events belong to the progress window
      if (e.kind === 'phase_start') {
        setCmpStages((prev) => {
          const done = prev.map((s) => (s.active ? { ...s, active: false, done: true } : s));
          const found = done.find((s) => s.phase === e.phase);
          if (found) return done.map((s) => (s.phase === e.phase ? { ...s, active: true, done: false, label: e.label ?? s.label } : s));
          return [...done, {
            phase: e.phase!, label: e.label ?? '', itemsDone: 0, itemsTotal: 0,
            bytesDone: 0, bytesTotal: 0, rate: 0, active: true, done: false,
          }];
        });
      } else if (e.kind === 'progress') {
        const ts = e.ts_ms ?? Date.now();
        const bd = e.bytes_done ?? 0;
        const prevR = cmpRate.current.get(e.phase);
        let ema = 0;
        if (prevR && ts > prevR.t) {
          const inst = ((bd - prevR.b) * 1000) / (ts - prevR.t);
          ema = prevR.ema > 0 ? prevR.ema * 0.7 + inst * 0.3 : inst;
          cmpRate.current.set(e.phase, { t: ts, b: bd, ema });
        } else if (!prevR) {
          cmpRate.current.set(e.phase, { t: ts, b: bd, ema: 0 });
        } else {
          ema = prevR.ema;
        }
        setCmpStages((prev) => {
          const patch = (s: CmpStage): CmpStage => ({
            ...s, label: '',
            itemsDone: e.items_done ?? 0, itemsTotal: e.items_total ?? 0,
            bytesDone: bd, bytesTotal: e.bytes_total ?? 0, rate: ema,
          });
          if (prev.some((s) => s.phase === e.phase)) return prev.map((s) => (s.phase === e.phase ? patch(s) : s));
          return [...prev, patch({
            phase: e.phase!, label: '', itemsDone: 0, itemsTotal: 0,
            bytesDone: 0, bytesTotal: 0, rate: 0, active: true, done: false,
          })];
        });
      }
    });
    return () => { un.then((f) => f()); };
  }, []);

  // P1: drag a directory onto a path box.
  // Tauri v2 has dragDropEnabled on by default, so HTML5 drop events never reach the webview — you must
  // go through onDragDropEvent, and payload.position is in **physical pixels**, to be converted yourself.
  useEffect(() => {
    let disposed = false;
    let dispose: (() => void) | undefined;
    getCurrentWebview().onDragDropEvent((ev) => {
      const p = ev.payload as unknown as { type: string; paths?: string[]; position?: { x: number; y: number } };
      if (p.type === 'leave') { setDropOn(null); return; }
      const pos = p.position;
      if (!pos) return;
      const r = window.devicePixelRatio || 1;
      const x = pos.x / r, y = pos.y / r;
      // While the editor is open only its fields count; otherwise the two roots on the main screen do.
      // Each region registers itself through a callback ref, so the editor's entry clears itself on
      // unmount and this stays a plain null check rather than a lookup that has to guess at markup.
      const scope = dropScope.current.editor ?? dropScope.current.path;
      const el = [...(scope?.querySelectorAll<HTMLInputElement>('input[data-drop]') ?? [])]
        .filter((n) => !n.disabled)
        .find((n) => {
          const b = n.getBoundingClientRect();
          return x >= b.left && x <= b.right && y >= b.top && y <= b.bottom;
        });
      const key = el?.dataset.root ?? el?.dataset.k ?? null;
      if (p.type === 'over' || p.type === 'enter') { setDropOn(key); return; }
      if (p.type !== 'drop') return;
      setDropOn(null);
      const first = p.paths?.[0];
      if (!el || !key || !first) return;
      void (async () => {
        // If a file was dropped, take its parent directory — a root field wants a directory
        let v = first;
        try {
          const info = await ipc.inspectPaths(first, '');
          if (info.source.exists && !info.source.is_dir) {
            const i = Math.max(v.lastIndexOf('\\'), v.lastIndexOf('/'));
            if (i > 0) v = v.slice(0, i);
          }
        } catch { /* if we can't tell, fill it in as-is */ }
        pushHistory(v);
        // Dropping on the two main-screen roots edits the job right away (same path as typing and
        // pressing Enter); in the editor it waits for save
        if (el.dataset.root === 'source' || el.dataset.root === 'target') {
          await saveRoot(el.dataset.root as 'source' | 'target', v);
        } else {
          editorApi.current?.setField(key, v);
          setStatus(`Filled in: ${v}`);
        }
      })();
    })
      // The unlisten handle can arrive after the effect has already been cleaned up (StrictMode
      // double-mounts in development); dropping it there would leak a second live handler
      .then((f) => { if (disposed) f(); else dispose = f; })
      .catch(() => { /* if drag and drop is unavailable, typed paths still work */ });
    return () => { disposed = true; dispose?.(); };
  }, [pushHistory, saveRoot, setStatus]);

  // Config pills follow the selected job
  useEffect(() => {
    if (!currentJob) { setCfgJob(null); return; }
    let live = true;
    ipc.getJob(currentJob.name).then((j) => { if (live) setCfgJob(j); }).catch(() => setCfgJob(null));
    return () => { live = false; };
  }, [currentJob]);

  // Watch (scheduled scans; seconds = near real time)
  const tickRef = useRef<() => void>(() => {});
  tickRef.current = async () => {
    if (!currentJob) { setWatchSecs(null); return; }
    const iv = (currentJob.watch_interval_secs ?? 30) * 1000;
    const left = watchNext.current - Date.now();
    if (left > 0) {
      if (!busy) setStatus(`Watching — next scan in ${Math.ceil(left / 1000)}s (${currentJob.name})`);
      return;
    }
    if (busy) return; // the previous round hasn't finished — skip this beat
    watchNext.current = Date.now() + iv;
    await doCompare();
  };

  useEffect(() => {
    if (watchSecs === null) return;
    const id = window.setInterval(() => tickRef.current(), 1000);
    return () => window.clearInterval(id);
  }, [watchSecs]);

  // Auto-apply is a separate effect so it observes the plan the compare above just produced
  const autoApplied = useRef<PlanDto | null>(null);
  useEffect(() => {
    if (watchSecs === null || busy || !plan || !currentJob || !currentJob.watch_auto_apply) return;
    if (plan.ops.length === 0 || autoApplied.current === plan) return;
    autoApplied.current = plan;
    void (async () => {
      const ops = plan.ops.filter((op) => selectable(op));
      setStatus(`Watch found ${ops.length} differences — running automatically…`);
      setBusy(true);
      try {
        await ipc.applyJobUnattended(currentJob.name, plan, ops, selTarget);
        refreshLastSyncs();
      } catch (e) {
        setStatus(`Auto-sync failed: ${e}`, 'err');
      }
      setBusy(false);
    })();
  }, [watchSecs, busy, plan, currentJob, selTarget, refreshLastSyncs, setStatus]);

  useEffect(() => {
    if (watchSecs === null || !plan || !currentJob || currentJob.watch_auto_apply) return;
    if (plan.ops.length > 0) setStatus(`Watch found ${plan.ops.length} differences`, 'err');
  }, [watchSecs, plan, currentJob, setStatus]);

  // Keyboard.
  //
  // Escape is deliberately absent: every overlay closes itself, and they stack, so one Escape
  // unwinds exactly one layer. Handling it here as well would close the sheet *and* the dialog
  // nested inside it on a single press.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      // While an overlay owns the screen it owns the keyboard: F5 must not kick off a compare
      // behind an open editor
      if (ctx || editor || settingsOpen || askSwap) return;
      if (confirmOpen) {
        if (e.key === 'Enter' && !(preflight && !preflight.ok && !acknowledged)) void doSync();
        return;
      }
      // F5 / F9 = the FFS compare / synchronize keys; Ctrl+R / Enter still work
      if (e.key === 'F5') { e.preventDefault(); void doCompare(); }
      else if (e.key === 'F9') { e.preventDefault(); void openConfirm(); }
      else if (mod && e.key.toLowerCase() === 'r') { e.preventDefault(); void doCompare(); }
      else if (e.key === 'Enter' && !(document.activeElement instanceof HTMLInputElement)) void openConfirm();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [ctx, editor, settingsOpen, askSwap, confirmOpen, preflight, acknowledged, doSync, doCompare, openConfirm]);

  // The funnel is anchored to its toolbar button and has no dismissal of its own; the context menu
  // brings its own (outside click, Esc, and any scroll under it), so it is not handled here
  useEffect(() => {
    const onClick = () => setFunnelAnchor(null);
    document.addEventListener('click', onClick);
    return () => document.removeEventListener('click', onClick);
  }, []);

  const hasRows = !!plan && plan.ops.length > 0;
  const confirmTotals: ConfirmTotals | null = useMemo(() => {
    if (!plan) return null;
    const t: ConfirmTotals = { copy: 0, update: 0, move: 0, del: 0, bytes: 0, delBytes: 0, flips: 0, hiddenChecked: 0 };
    for (const i of final) {
      const op = eff(plan, flipped, i);
      if (op.action === 'copy') { t.copy++; t.bytes += op.size ?? 0; }
      else if (op.action === 'update') { t.update++; t.bytes += op.size ?? 0; }
      else if (op.action === 'move') t.move++;
      else if (op.action === 'delete' || op.action === 'delete_dir') { t.del++; t.delBytes += op.size ?? 0; }
    }
    t.flips = flipped.filter(Boolean).length;
    t.hiddenChecked = checked.filter(Boolean).length - final.length;
    return t;
  }, [plan, final, flipped, checked]);

  return (
    <>
      <div className="dragstrip" data-tauri-drag-region />
      <div className="app">
        <Sidebar
          jobs={jobs}
          currentName={currentJob?.name ?? null}
          lastMap={lastMap}
          busy={busy}
          appVersion={appVersion}
          jobsDir={jobsDir}
          onSelect={selectJob}
          onEdit={(name) => setEditor({ name })}
          onNew={() => setEditor({ name: null })}
        />
        <main className="main">
          <Toolbar
            job={currentJob}
            hasPlan={!!plan}
            finalCount={final.length}
            stats={stats}
            busy={busy}
            canSync={hasRows}
            watchSecs={watchSecs}
            onCompare={() => void doCompare()}
            onSync={() => void openConfirm()}
            onEditGroup={(g) => { if (currentJob) setEditor({ name: currentJob.name, focusGroup: g }); }}
            onToggleLog={() => setLogOpen((v) => !v)}
            onToggleWatch={() => {
              if (watchSecs !== null) { setWatchSecs(null); setStatus('Watch stopped'); return; }
              if (!currentJob) return;
              const iv = currentJob.watch_interval_secs ?? 30;
              watchNext.current = Date.now() + iv * 1000;
              setWatchSecs(iv);
              setStatus(`Watch on: compare every ${iv}s (the hash cache means an unchanged tree costs only the walk)${currentJob.watch_auto_apply ? ' · auto-run' : ''}`);
            }}
          />
          <PathLine
            job={currentJob}
            cfgJob={cfgJob}
            busy={busy}
            selTarget={selTarget}
            pathHistory={pathHistory}
            dropOn={dropOn === 'source' || dropOn === 'target' ? dropOn : null}
            scopeRef={(el) => { dropScope.current.path = el; }}
            onCommit={(which, v) => void saveRoot(which, v)}
            onBrowse={(which) => void browseRoot(which)}
            onSwap={() => void requestSwap()}
            onSelectTarget={(i) => {
              // A different target means a different comparison, so the old plan is stale
              setSelTarget(i);
              setPlan(null);
              setChecked([]);
              setFlipped([]);
              const t = (currentJob?.targets ?? [])[i] ?? '';
              setStatus(`Switched target → ${t} — Compare again (Ctrl+R)`);
            }}
            onEditGroup={(g) => { if (currentJob) setEditor({ name: currentJob.name, focusGroup: g }); }}
          />
          {hasRows && !sameOpen && !cmpActive && (
            <FilterBar
              plan={plan!}
              flipped={flipped}
              chips={chips}
              onChips={setChips}
              onSearch={setSearch}
              searchKey={currentJob?.name ?? ''}
              funnelCount={funnelActive(vfilter)}
              funnelOpen={!!funnelAnchor}
              sameOpen={sameOpen}
              grouped={grouped}
              sort={sort}
              anyCollapsed={collapsedDirs.size > 0}
              pathMode={pathMode}
              onToggleFunnel={(a) => setFunnelAnchor((cur) => (cur ? null : a))}
              onToggleSame={() => {
                if (!plan) { setStatus('Compare first — identical items come from the last compare snapshot', 'err'); return; }
                setSameOpen((v) => !v);
              }}
              onExportCsv={() => void exportCsv()}
              onToggleFold={() => setCollapsedDirs((prev) => (prev.size > 0 ? new Set() : new Set(groupDirs)))}
              onToggleGroup={() => {
                // Clicking it while sorted clears the sort and returns to grouping, rather than adding
                // another toggle on top of "sorted + flat"
                if (sort) setSort(null);
                else {
                  const next = !grouped;
                  setGrouped(next);
                  localStorage.setItem('sd.grouped', next ? 'on' : 'off');
                }
                setCollapsedDirs(new Set());
              }}
              onTogglePathMode={() => {
                const next = pathMode === 'rel' ? 'full' : 'rel';
                setPathMode(next);
                localStorage.setItem('sd.pathmode', next);
              }}
            />
          )}
          <div className="reviewrow">
            <Overview
              plan={plan}
              flipped={flipped}
              collapsed={ovCollapsed}
              ovFilter={ovFilter}
              expanded={ovExpanded}
              onToggleCollapsed={() => setOvCollapsed((v) => {
                localStorage.setItem('sd.ov', v ? 'open' : 'closed');
                return !v;
              })}
              onFilter={setOvFilter}
              onToggleExpanded={(k) => setOvExpanded((prev) => {
                const next = new Set(prev);
                if (next.has(k)) next.delete(k); else next.add(k);
                return next;
              })}
            />
            {/* A callback ref into state, not a useRef: the table measures this element, and a child's
                effects run before an ancestor host ref attaches — so on mount a ref would still be
                null exactly when the virtual window first needs it */}
            <div className="tablewrap" ref={setTableWrap}>
              {cmpActive ? (
                <ComparePanel
                  stages={cmpStages}
                  cancelling={cmpCancelling}
                  onCancel={() => {
                    setCmpCancelling(true);
                    ipc.cancelRun().catch(() => {});
                    setStatus('Cancelling the compare…');
                  }}
                />
              ) : sameOpen && currentJob ? (
                <SamePanel jobName={currentJob.name} onClose={() => setSameOpen(false)} />
              ) : hasRows ? (
                <PlanTable
                  plan={plan!}
                  flipped={flipped}
                  checked={checked}
                  rowPlan={rowPlan}
                  visible={visible}
                  pathMode={pathMode}
                  sort={sort}
                  collapsedDirs={collapsedDirs}
                  wrap={tableWrap}
                  resetKey={`${currentJob?.name}|${search}|${[...chips].join()}|${ovFilter}|${sort?.key}${sort?.dir}`}
                  onToggleRow={toggleRow}
                  onToggleMany={toggleMany}
                  onFlip={flipRow}
                  onFoldDir={foldDir}
                  onSort={toggleSort}
                  onContextRow={rowMenu}
                />
              ) : plan ? (
                <Placeholder
                  icon={<CircleCheck size={26} className="icon-ok" />}
                  title="Both sides are identical"
                  description="Nothing to synchronize. The status bar below counts what was scanned and what a filter excluded."
                />
              ) : (
                <Placeholder
                  icon={<FolderSearch size={26} />}
                  title={currentJob ? `Ready — ${currentJob.name}` : 'No job selected'}
                  description={
                    currentJob
                      ? 'Press Compare (F5 or Ctrl+R) to walk both roots and build a plan.'
                      : 'Pick a job on the left, then press Compare (F5 or Ctrl+R).'
                  }
                />
              )}
            </div>
          </div>
          {logOpen && (
            <LogPanel
              jobName={currentJob?.name ?? null}
              reloadKey={logReload}
              onClose={() => setLogOpen(false)}
              onSettings={() => setSettingsOpen(true)}
              onStatus={setStatus}
            />
          )}
          <StatusBar
            status={status}
            onUndo={runUndo}
            plan={plan}
            visibleCount={visible.length}
            zoom={zoom.zoom}
            onZoomIn={zoom.zoomIn}
            onZoomOut={zoom.zoomOut}
            onZoomReset={zoom.zoomReset}
          />
        </main>
      </div>

      {ctx && (
        <ContextMenu at={ctx} onClose={() => setCtx(null)}>
          {ctx.items.map((it, k) => (it.sep
            ? <MenuDivider key={k} />
            : <MenuItem key={k} disabled={it.disabled} danger={it.danger} onClick={it.run}>{it.label}</MenuItem>
          ))}
        </ContextMenu>
      )}
      {funnelAnchor && plan && (
        <FunnelPopover
          anchor={funnelAnchor}
          vfilter={vfilter}
          shown={visible.length}
          planned={plan.ops.length}
          onChange={(next) => {
            setVfilter(next);
            if (next.masks.join('\n') !== vfilter.masks.join('\n')) void recomputeMasks(plan, flipped, next.masks);
          }}
          onClear={() => { setVfilter(EMPTY_FILTER); setMaskHit([]); }}
          onPromote={(masks) => {
            if (!masks.length) { setStatus('Write at least one mask first', 'err'); return; }
            setFunnelAnchor(null);
            void addExcludes(masks, 'Written into the exclude list');
          }}
          onDone={() => setFunnelAnchor(null)}
        />
      )}
      {editor && (
        <JobEditor
          name={editor.name}
          focusGroup={editor.focusGroup}
          dropOn={dropOn}
          scopeRef={(el) => { dropScope.current.editor = el; }}
          apiRef={editorApi}
          onClose={() => setEditor(null)}
          onSaved={async (name, job) => {
            setEditor(null);
            pushHistory(job.source);
            pushHistory(job.target);
            await refreshJobs(currentJob?.name === name ? name : undefined);
            if (currentJob?.name === name) setCfgJob(job);
            setStatus(`Saved '${name}'`, 'ok');
          }}
          onDeleted={async (name) => {
            setEditor(null);
            if (currentJob?.name === name) { setCurrentJob(null); setPlan(null); setChecked([]); setFlipped([]); }
            await refreshJobs();
            setStatus(`Deleted '${name}'`);
          }}
          onStatus={setStatus}
        />
      )}
      {settingsOpen && (
        <SettingsSheet
          onClose={() => setSettingsOpen(false)}
          onSaved={(msg, cls) => { setSettingsOpen(false); setStatus(msg, cls); setLogReload((k) => k + 1); }}
          onStatus={setStatus}
        />
      )}
      {askSwap && (
        <ConfirmDialog
          title={`Swap the two roots of '${askSwap.name}'?`}
          message={
            `source ← ${askSwap.job.target}\ntarget ← ${askSwap.job.source}\n\n` +
            (askSwap.job.mode === 'mirror'
              ? 'In mirror mode this reverses which side is authoritative: after the swap, the original target wins.\n\n'
              : '') +
            'The job file is rewritten and the current compare result is discarded. The status bar keeps an undo.'
          }
          actions={[{ label: 'Swap them', onConfirm: () => void doSwap(askSwap.name, askSwap.job) }]}
          onCancel={() => setAskSwap(null)}
        />
      )}
      {confirmOpen && currentJob && confirmTotals && (
        <ConfirmSheet
          job={currentJob}
          totals={confirmTotals}
          preflight={preflight}
          preflightError={preflightError}
          acknowledged={acknowledged}
          onAcknowledge={setAcknowledged}
          onCancel={() => setConfirmOpen(false)}
          onConfirm={() => void doSync()}
        />
      )}
    </>
  );
}
