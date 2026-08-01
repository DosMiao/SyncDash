// SyncDash main window.
//
// This component owns the session state (selected job, compare result, check/flip vectors, view filters)
// and every action that crosses the Tauri boundary; everything under components/ is presentation fed by
// props. The frontend derives a flipped row for review, but preflight/apply receive only row indices
// and flip flags; Rust reconstructs the executable operations from the authenticated plan.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { CircleCheck, FolderSearch } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';
import { getCurrentWebview } from '@tauri-apps/api/webview';

import * as ipc from '../core/ipc';
import { EMPTY_FILTER, computeVisible, finalIdx, funnelActive } from '../core/filter';
import { buildLayout, flattenLayout, layoutDirs, treeDirOf } from '../core/grouping';
import { addExcludeEntries } from '../core/junk';
import { baseOf, fullPath, p2 } from '../core/format';
import { canFlip, eff, keySpec, metaOf, selectable, selectedRows, sidePaths } from '../core/plan';
import { reduceCompareStages } from '../core/compareProgress';
import type { Chip, PlanDto, Sort, SortKey } from '../core/plan';
import type { CmpStage, CompareProgressEvent } from '../core/compareProgress';
import type { ViewFilter } from '../core/filter';
import type { PlanLayout } from '../core/grouping';
import type { JobDto } from '../core/types/generated/JobDto';
import type { PreflightDto } from '../core/types/generated/PreflightDto';
import type { RunRecord } from '../core/types/generated/RunRecord';

import { useStatus } from './hooks/useStatus';
import { useZoomControl } from './hooks/useZoomControl';
import {
  activeSession as sessionForSelection,
  invalidateJobSession,
  ownerMatchesSelection,
  reconcileRefreshedJobSession,
  reconcileSavedJobSession,
  successfulSession,
  targetForSelection,
} from './state/compare-session';
import type { CompareSession } from './state/compare-session';
import { ComparePanel } from './components/ComparePanel';
import { ScanFaultBanner } from './components/ScanFaultBanner';
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
import type { ConfirmTotals } from './components/ConfirmSheet';
import type { EditorApi } from './components/JobEditor';

const HIST_KEY = 'sd.pathhist';

/// Stable identity for "no plan, nothing to lay out" — a fresh object literal here would make the
/// flatten memo below recompute on every render
const EMPTY_LAYOUT: PlanLayout = { order: [], tree: null };
const EMPTY_FLAGS: boolean[] = [];

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
  const [compareSession, setCompareSession] = useState<CompareSession | null>(null);
  const [maskHit, setMaskHit] = useState<boolean[]>([]);
  const [busy, setBusy] = useState(false);
  const syncInFlight = useRef(false);
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
  // Stable identities: a ref callback whose identity changes is detached with null and reattached
  // on every render, and these two are handed to components that re-render on every keystroke.
  const setPathScope = useCallback((el: HTMLElement | null) => { dropScope.current.path = el; }, []);
  const setEditorScope = useCallback((el: HTMLElement | null) => { dropScope.current.editor = el; }, []);

  // Compare progress (fed by the run-progress stream)
  const [cmpActive, setCmpActive] = useState(false);
  const [cmpStages, setCmpStages] = useState<CmpStage[]>([]);
  const [cmpCancelling, setCmpCancelling] = useState(false);
  /// Rate EMA (0.7 old + 0.3 new): the instantaneous rate swings wildly with file size
  const cmpRate = useRef(new Map<string, { t: number; b: number; ema: number }>());
  const cmpRunId = useRef(-1);

  // AutoScan (the job field behind it is watch_interval_secs)
  const [watchSecs, setWatchSecs] = useState<number | null>(null);
  const watchNext = useRef(0);

  const editorApi = useRef<EditorApi | null>(null);
  const { status, set: setStatus, withUndo: setStatusUndo, runUndo } = useStatus('');
  const zoom = useZoomControl();
  const selectionRef = useRef<{ job: JobDto | null; targetIndex: number }>({ job: null, targetIndex: 0 });
  selectionRef.current = { job: currentJob, targetIndex: selTarget };

  // Derived view

  const activeCompare = sessionForSelection(compareSession, currentJob, selTarget);
  const plan = activeCompare?.plan ?? null;
  const checked = activeCompare?.checked ?? EMPTY_FLAGS;
  const flipped = activeCompare?.flipped ?? EMPTY_FLAGS;

  // Three memos, not one, because the three questions change at different rates. Membership is the
  // expensive full-table scan and no longer depends on `sort`, so clicking a header does not re-run
  // it; the layout does the sorting; flattening only decides which member rows a fold emits, so
  // folding one directory costs one pass instead of redoing the sort.
  //
  // `flipped` legitimately appears in all three: a flip changes eff(op), hence the row's directory
  // and its side paths, hence its group and its sort key. It is a full rebuild by necessity.
  const visible = useMemo(() => (
    plan ? computeVisible({ plan, flipped, chips, search, ovFilter, vfilter, maskHit }) : []
  ), [plan, flipped, chips, search, ovFilter, vfilter, maskHit]);

  const final = useMemo(() => finalIdx(visible, checked), [visible, checked]);

  const layout = useMemo(() => (
    plan ? buildLayout({ plan, flipped, visible, grouped, sort }) : EMPTY_LAYOUT
  ), [plan, flipped, visible, grouped, sort]);

  const rowPlan = useMemo(() => flattenLayout(layout, collapsedDirs), [layout, collapsedDirs]);
  const treeDirs = useMemo(() => layoutDirs(layout), [layout]);
  // A filter may temporarily remove a collapsed branch. Only keys present in this layout decide
  // whether the toolbar says Expand all; otherwise one stale path leaves the control backwards.
  const anyCollapsed = useMemo(
    () => treeDirs.some((dir) => collapsedDirs.has(dir)),
    [treeDirs, collapsedDirs],
  );

  // Fold state belongs to one compare result. A new plan can reuse the same relative names for
  // entirely different roots, so carrying old folds over would hide fresh results on arrival.
  useEffect(() => { setCollapsedDirs(new Set()); }, [plan]);

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
    if (keepName) {
      // listJobs is the authoritative registry. The name guard prevents a delayed refresh from
      // hijacking a newer selection; when that guarded job disappeared, retaining `cur` would
      // instead leave a ghost row that every later Compare retries.
      setCurrentJob((cur) => (cur?.name === keepName ? list.find((x) => x.name === keepName) ?? null : cur));
    }
    return list;
  }, []);

  const refreshLastSyncs = useCallback(() => {
    ipc.lastSyncs().then(setLastMap).catch(() => { /* missing logs are not fatal */ });
  }, []);

  const resetSafetyUi = useCallback(() => {
    setAcknowledged(false);
    setConfirmOpen(false);
    setPreflight(null);
    setPreflightError(null);
    setSameOpen(false);
    setFunnelAnchor(null);
    setCtx(null);
    setAskSwap(null);
  }, []);

  const resetNavigationUi = useCallback(() => {
    resetSafetyUi();
    setChips(new Set());
    setSearch('');
    setOvFilter(null);
    setOvExpanded(new Set());
    setSort(null);
    setVfilter(EMPTY_FILTER);
    setMaskHit([]);
  }, [resetSafetyUi]);

  useEffect(() => {
    if (!currentJob || currentJob.targets.length === 0 || selTarget < currentJob.targets.length) return;
    setSelTarget(0);
    resetNavigationUi();
    setStatus(`'${currentJob.name}' no longer has target ${selTarget + 1}; selected target 1`);
  }, [currentJob, selTarget, resetNavigationUi, setStatus]);

  const invalidateCompareForJob = useCallback((name: string) => {
    setCompareSession((session) => invalidateJobSession(session, name));
  }, []);

  const setChecked = useCallback((next: boolean[] | ((prev: boolean[]) => boolean[])) => {
    setCompareSession((session) => {
      if (!session || !ownerMatchesSelection(session.plan.owner, currentJob, selTarget)) return session;
      const checked = typeof next === 'function' ? next(session.checked) : next;
      return { ...session, checked };
    });
  }, [currentJob, selTarget]);

  const setFlipped = useCallback((next: boolean[] | ((prev: boolean[]) => boolean[])) => {
    setCompareSession((session) => {
      if (!session || !ownerMatchesSelection(session.plan.owner, currentJob, selTarget)) return session;
      const flipped = typeof next === 'function' ? next(session.flipped) : next;
      return { ...session, flipped };
    });
  }, [currentJob, selTarget]);

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
    if (!currentJob || busy || editor) return;
    const name = currentJob.name;
    const targetIndex = selTarget;
    resetSafetyUi();
    setBusy(true);
    setStatus(`Comparing '${name}' ...`);
    setCmpStages([]);
    cmpRate.current.clear();
    setCmpCancelling(false);
    setCmpActive(true);
    try {
      const p = await ipc.compareJob(name, targetIndex);
      const f = p.ops.map(() => false);
      setCompareSession(successfulSession(p, p.ops.map((op) => selectable(op)), f));
      setChips(new Set());
      setOvFilter(null);
      setOvExpanded(new Set());
      setSort(null);
      // A job file can be edited outside the app while it is open. Compare used the authoritative
      // file, so refresh the list row before deciding whether the returned owner belongs on screen.
      // Snapshot first: refreshJobs may commit the new row (or null) before this continuation runs,
      // and then the ref no longer tells us that the selected job changed underneath this compare.
      const selectedBeforeRefresh = selectionRef.current;
      let refreshedJob: JobDto | null = null;
      let refreshProblem: unknown = null;
      try {
        const list = await refreshJobs(name);
        refreshedJob = list.find((job) => job.name === name) ?? null;
        setCompareSession((session) => reconcileRefreshedJobSession(session, name, refreshedJob));
      } catch (e) {
        refreshProblem = e;
      }
      const selected = selectionRef.current;
      const selectedJob = selected.job?.name === name && !refreshProblem ? refreshedJob : selected.job;
      let navigationWasReset = false;
      if (selectedBeforeRefresh.job?.name === name && !refreshProblem) {
        if (!refreshedJob) {
          setSelTarget(0);
          setWatchSecs(null);
          resetNavigationUi();
          navigationWasReset = true;
        } else if (selectedBeforeRefresh.job.config_revision !== refreshedJob.config_revision) {
          resetNavigationUi();
          navigationWasReset = true;
        }
      }
      const visibleHere = ownerMatchesSelection(p.owner, selectedJob, selected.targetIndex);
      if (visibleHere) {
        // resetNavigationUi cleared both the funnel and maskHit. Replaying masks from this render's
        // stale closure would hide rows behind an apparently empty funnel after an external edit.
        await recomputeMasks(p, f, navigationWasReset ? [] : vfilter.masks);
      } else {
        setMaskHit([]);
      }
      // The counts bar to the right already says what was scanned and what is showing, and the
      // placeholder in the table says "identical" in full — this line only has to say the result
      if (visibleHere) {
        setStatus(
          p.ops.length === 0
            ? 'Both sides identical'
            : `${p.ops.length} items · ${p.header.conflict_count} conflicts`,
          p.header.conflict_count > 0 ? 'err' : '',
        );
      } else if (refreshProblem) {
        setStatus(`Compare finished for '${name}', but the refreshed job identity could not be read: ${refreshProblem}`, 'err');
      } else {
        setStatus(`Compare finished for '${name}', but its job or target changed — the result was not attached to the current view`, 'err');
      }
    } catch (e) {
      const cancelled = String(e) === 'cancelled';
      let suffix = '';
      let refreshProblem: unknown = null;
      try {
        // Compare reads the job file directly. If that read failed because the file was edited,
        // removed, or had its targets changed outside the app, refresh even on failure so the
        // stale list row cannot trap every subsequent attempt on the same invalid selection.
        const selected = selectionRef.current;
        const list = await refreshJobs(name);
        const refreshedJob = list.find((job) => job.name === name) ?? null;
        setCompareSession((session) => reconcileRefreshedJobSession(session, name, refreshedJob));
        if (selected.job?.name === name) {
          if (!refreshedJob) {
            setSelTarget(0);
            setWatchSecs(null);
            resetNavigationUi();
            suffix = ` · '${name}' is no longer a registered job`;
          } else if (selected.job.config_revision !== refreshedJob.config_revision) {
            resetNavigationUi();
            suffix = ' · refreshed the changed job configuration';
          }
        }
      } catch (refreshError) {
        refreshProblem = refreshError;
      }
      const base = cancelled ? 'Compare cancelled' : `Compare failed: ${e}`;
      if (refreshProblem) suffix = ` · job-list refresh failed: ${refreshProblem}`;
      setStatus(`${base}${suffix}`, cancelled && !refreshProblem ? '' : 'err');
    } finally {
      setCmpActive(false);
      setBusy(false);
    }
  }, [currentJob, busy, editor, selTarget, vfilter.masks, recomputeMasks, refreshJobs, resetNavigationUi, resetSafetyUi, setStatus]);

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
      setPreflight(await ipc.preflight(currentJob.name, plan, selectedRows(final, flipped), acknowledged, selTarget));
    } catch (e) {
      setPreflightError(String(e));
    }
  }, [currentJob, plan, busy, checked, final, flipped, acknowledged, selTarget, setStatus]);

  const doSync = useCallback(async () => {
    setConfirmOpen(false);
    if (!currentJob || !plan || busy || syncInFlight.current) return;
    syncInFlight.current = true;
    const selected = selectedRows(final, flipped);
    setBusy(true);
    setStatus(`Synchronizing '${currentJob.name}' (${selected.length} items)...`);
    // Whether the progress window stays during a sync is its own Auto-close / When-finished business
    let launchId: number | null = null;
    try {
      // The command returns only after the new window has installed its run-progress listener.
      // Starting apply any earlier loses the phase start/totals on a freshly opened window.
      launchId = await ipc.openProgressWindow();
      // Once apply is invoked, any rejection may still follow partial writes. Retire this plan before
      // crossing that boundary; only the post-run compare is entitled to publish another one.
      invalidateCompareForJob(currentJob.name);
      resetSafetyUi();
      const r = await ipc.applyJob(currentJob.name, plan, selected, acknowledged, selTarget, launchId);
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
    } finally {
      if (launchId !== null) void ipc.cancelProgressLaunch(launchId);
      syncInFlight.current = false;
    }
  }, [currentJob, plan, busy, final, flipped, acknowledged, selTarget, doCompare, refreshLastSyncs, invalidateCompareForJob, resetSafetyUi, setStatus]);

  const selectJob = useCallback((j: JobDto) => {
    if (currentJob?.name === j.name) return;
    const targetIndex = targetForSelection(compareSession, j);
    const restored = sessionForSelection(compareSession, j, targetIndex);
    setCurrentJob(j);
    setSelTarget(targetIndex);
    resetNavigationUi();
    setWatchSecs(null);
    setStatus(restored
      ? `${j.name} · restored ${restored.plan.ops.length} compare items`
      : `${j.name} · ${j.mode}${j.rigor !== 'standard' ? ` · ${j.rigor}` : ''}`);
  }, [currentJob?.name, compareSession, resetNavigationUi, setStatus]);

  /// Write a root edited on the main screen back to the job TOML. Changing a root invalidates the current
  /// plan, so clear it too. For multi-target jobs, only the currently selected target changes.
  const saveRoot = useCallback(async (which: 'source' | 'target', value: string) => {
    if (!currentJob) return;
    const name = currentJob.name;
    const v = value.trim();
    if (!v) return;
    let j: ipc.JobFull;
    try {
      j = await ipc.getJob(name);
    } catch (e) {
      setStatus(`Failed to read the job before changing ${which}: ${e}`, 'err');
      return;
    }
    const ts = j.targets && j.targets.length ? [...j.targets] : [j.target];
    const before = which === 'source' ? j.source : ts[selTarget];
    if (before === v) return; // unchanged means no disk write — otherwise every blur would bump the mtime
    const next: ipc.JobFull = which === 'source'
      ? { ...j, source: v }
      : { ...j, target: selTarget === 0 ? v : j.target, targets: ts.map((t, i) => (i === selTarget ? v : t)) };
    try {
      await ipc.saveJob(name, next);
    } catch (e) {
      setStatus(`Failed to write ${which} back to the job: ${e}`, 'err');
      return;
    }
    // The mutation is committed at this point. Retire its old compare immediately, and never let a
    // later list-refresh failure make the UI claim that this successful disk write failed.
    invalidateCompareForJob(name);
    resetSafetyUi();
    pushHistory(v);
    const undo = async () => {
      const cur = await ipc.getJob(name);
      const back: ipc.JobFull = which === 'source'
        ? { ...cur, source: before }
        : { ...cur, target: selTarget === 0 ? before : cur.target, targets: (cur.targets ?? []).map((t, i) => (i === selTarget ? before : t)) };
      await ipc.saveJob(name, back);
      invalidateCompareForJob(name);
      resetSafetyUi();
      try {
        await refreshJobs(name);
        setStatus(`Restored ${which}`);
      } catch (e) {
        setStatus(`Restored ${which}, but refreshing the job list failed: ${e}`, 'err');
      }
    };
    const success = `Changed ${which} → ${v} — Compare again (Ctrl+R)`;
    try {
      await refreshJobs(name);
      setStatusUndo(success, 'Undo', undo);
    } catch (e) {
      setStatusUndo(`${success}; refreshing the job list failed: ${e}`, 'Undo', undo, 'err');
    }
  }, [currentJob, selTarget, pushHistory, refreshJobs, invalidateCompareForJob, resetSafetyUi, setStatus, setStatusUndo]);

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
    } catch (e) {
      setStatus(`Swap failed: ${e}`, 'err');
      return;
    }
    invalidateCompareForJob(name);
    resetSafetyUi();
    setChips(new Set());
    setOvFilter(null);
    setOvExpanded(new Set());
    const undo = async () => {
      const cur = await ipc.getJob(name);
      await ipc.saveJob(name, { ...cur, ...prev });
      invalidateCompareForJob(name);
      resetSafetyUi();
      try {
        await refreshJobs(name);
        setStatus(`Restored the two roots of '${name}'`);
      } catch (e) {
        setStatus(`Restored the two roots of '${name}', but refreshing the job list failed: ${e}`, 'err');
      }
    };
    const success = `Swapped the two roots of '${name}' — Compare again (Ctrl+R)`;
    try {
      await refreshJobs(name);
      setStatusUndo(success, 'Undo swap', undo);
    } catch (e) {
      setStatusUndo(`${success}; refreshing the job list failed: ${e}`, 'Undo swap', undo, 'err');
    }
  }, [refreshJobs, invalidateCompareForJob, resetSafetyUi, setStatus, setStatusUndo]);

  /// Write an exclude back into the job's exclude list. Pruning during the scan only takes effect at the
  /// next Compare, so the message has to say so and leave an undo behind.
  const addExcludes = useCallback(async (masks: string[], label: string) => {
    if (!currentJob) { setStatus('Select a job first', 'err'); return; }
    const name = currentJob.name;
    let j: ipc.JobFull;
    try {
      j = await ipc.getJob(name);
    } catch (e) {
      setStatus(`Failed to read the job before adding the exclude: ${e}`, 'err');
      return;
    }
    const prev = [...j.exclude];
    // Folded the way the engine folds, not by string equality: a mask typed with backslashes, in a
    // different case, or in NFD is the same rule to the filter, and appending it again would leave
    // two lines that mean one thing — and a preset box unticked next to its own pattern
    const { next, added: add } = addExcludeEntries(prev, masks);
    if (!add.length) { setStatus(`The job already has ${masks.length > 1 ? 'all of these masks' : 'this exclude'}`); return; }
    try {
      await ipc.saveJob(name, { ...j, exclude: next });
    } catch (e) {
      setStatus(`Failed to write exclude: ${e}`, 'err');
      return;
    }
    invalidateCompareForJob(name);
    resetSafetyUi();
    const undo = async () => {
      const cur = await ipc.getJob(name);
      await ipc.saveJob(name, { ...cur, exclude: prev });
      invalidateCompareForJob(name);
      resetSafetyUi();
      try {
        await refreshJobs(name);
        setStatus('Exclude undone');
      } catch (e) {
        setStatus(`Exclude undone, but refreshing the job list failed: ${e}`, 'err');
      }
    };
    const success = `${label}: ${add.join(', ')} — Compare again to build a result with this exclusion`;
    try {
      await refreshJobs(name);
      setStatusUndo(success, 'Undo exclude', undo);
    } catch (e) {
      setStatusUndo(`${success}; refreshing the job list failed: ${e}`, 'Undo exclude', undo, 'err');
    }
  }, [currentJob, refreshJobs, invalidateCompareForJob, resetSafetyUi, setStatus, setStatusUndo]);

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
      // layout.order, not `visible`: the export is a snapshot of the view, so it follows the sort
      // and the directory grouping you are looking at
      const n = await ipc.exportCsv(
        path, plan.header,
        layout.order.map((i) => eff(plan, flipped, i)),
        layout.order.map((i) => metaOf(plan, i)),
        layout.order.map((i) => checked[i]),
      );
      setStatusUndo(`Exported ${n} rows to ${path}`, 'Open containing folder', () => ipc.reveal(path));
    } catch (e) {
      setStatus(`Export failed: ${e}`, 'err');
    }
  }, [plan, currentJob, visible, layout, flipped, checked, setStatus, setStatusUndo]);

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
  }, [setChecked]);

  const toggleMany = useCallback((items: number[], v: boolean) => {
    setChecked((prev) => { const next = [...prev]; for (const i of items) next[i] = v; return next; });
  }, [setChecked]);

  const flipRow = useCallback((i: number) => {
    setFlipped((prev) => { const next = [...prev]; next[i] = !next[i]; return next; });
  }, [setFlipped]);

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
      const { natural } = keySpec(key);
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
    const dir = treeDirOf(op);
    const sameDir = visible.filter((k) => {
      const candidate = treeDirOf(eff(plan, flipped, k));
      // `(root)` remains a direct-files bucket; a real folder owns its complete visible subtree.
      const inTree = dir === '' ? candidate === '' : candidate === dir || candidate.startsWith(`${dir}/`);
      return inTree && selectable(eff(plan, flipped, k));
    });
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
        { label: `${dir ? 'Uncheck this folder and subfolders' : 'Uncheck root-level items'} (${sameDir.length})`, disabled: sameDir.length === 0, run: () => toggleMany(sameDir, false) },
      ],
    });
  }, [plan, flipped, visible, currentJob, addExcludes, flipRow, toggleMany, setChecked, setStatus]);

  // Init

  useEffect(() => {
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
    const un = listen<CompareProgressEvent>('run-progress', (ev) => {
      const e = ev.payload;
      if (e.purpose !== 'compare' || e.run_id < cmpRunId.current) return;
      if (e.run_id > cmpRunId.current) {
        cmpRunId.current = e.run_id;
        cmpRate.current.clear();
        setCmpStages([]);
      }
      // A `log` event carries no phase, so the phase guard below would drop it. Errors do carry
      // one, but the guard used to sit above every branch and there was no branch to reach:
      // `phase_start` and `progress` were the only two, and a scan that could not read a directory
      // produced an event nothing was listening for.
      if (e.kind === 'error') {
        setStatus(`${e.action === 'walk' ? 'Scan could not read' : 'Error'}: ${e.message ?? ''}`, 'err');
        return;
      }
      if (!e.phase) return;
      if (e.kind === 'phase_start') {
        setCmpStages((prev) => reduceCompareStages(prev, e));
      } else if (e.kind === 'totals') {
        const ts = e.ts_ms ?? Date.now();
        const bd = e.bytes_done ?? 0;
        cmpRate.current.set(e.phase, { t: ts, b: bd, ema: 0 });
        setCmpStages((prev) => reduceCompareStages(prev, e));
      } else if (e.kind === 'progress') {
        const ts = e.ts_ms ?? Date.now();
        const bd = e.bytes_done ?? 0;
        const prevR = cmpRate.current.get(e.phase);
        let ema = 0;
        if (prevR && ts > prevR.t && bd >= prevR.b) {
          const inst = ((bd - prevR.b) * 1000) / (ts - prevR.t);
          ema = prevR.ema > 0 ? prevR.ema * 0.7 + inst * 0.3 : inst;
          cmpRate.current.set(e.phase, { t: ts, b: bd, ema });
        } else if (!prevR) {
          cmpRate.current.set(e.phase, { t: ts, b: bd, ema: 0 });
        } else {
          ema = prevR.ema;
        }
        setCmpStages((prev) => reduceCompareStages(prev, e, ema));
      } else if (e.kind === 'phase_end') {
        setCmpStages((prev) => reduceCompareStages(prev, e));
      }
    });
    return () => { un.then((f) => f()); };
  }, [setStatus]);

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

  // AutoScan (scheduled scans; seconds = near real time)
  const tickRef = useRef<() => void>(() => {});
  tickRef.current = async () => {
    if (!currentJob) { setWatchSecs(null); return; }
    const iv = (currentJob.watch_interval_secs ?? 30) * 1000;
    const left = watchNext.current - Date.now();
    if (left > 0) {
      if (!busy) setStatus(`AutoScan — next scan in ${Math.ceil(left / 1000)}s (${currentJob.name})`);
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
      const selected = selectedRows(
        plan.ops.flatMap((op, index) => (selectable(op) ? [index] : [])),
        EMPTY_FLAGS,
      );
      if (selected.length === 0) {
        setStatus(
          `AutoScan found ${plan.ops.length} conflicts/notes, but no executable differences — review required`,
          'err',
        );
        return;
      }
      setStatus(`AutoScan found ${selected.length} differences — running automatically…`);
      setBusy(true);
      invalidateCompareForJob(currentJob.name);
      resetSafetyUi();
      try {
        await ipc.applyJobUnattended(currentJob.name, plan, selected, selTarget);
        refreshLastSyncs();
      } catch (e) {
        setStatus(`Auto-sync failed: ${e}`, 'err');
      }
      setBusy(false);
    })();
  }, [watchSecs, busy, plan, currentJob, selTarget, refreshLastSyncs, invalidateCompareForJob, resetSafetyUi, setStatus]);

  useEffect(() => {
    if (watchSecs === null || !plan || !currentJob || currentJob.watch_auto_apply) return;
    if (plan.ops.length > 0) setStatus(`AutoScan found ${plan.ops.length} differences`, 'err');
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
      <div className="app">
        <Sidebar
          jobs={jobs}
          currentName={currentJob?.name ?? null}
          lastMap={lastMap}
          busy={busy}
          appVersion={appVersion}
          jobsDir={jobsDir}
          onSelect={selectJob}
          onEdit={(name) => { if (!busy) setEditor({ name }); }}
          onNew={() => { if (!busy) setEditor({ name: null }); }}
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
            onToggleLog={() => setLogOpen((v) => !v)}
            onToggleWatch={() => {
              if (watchSecs !== null) { setWatchSecs(null); setStatus('AutoScan stopped'); return; }
              if (!currentJob) return;
              const iv = currentJob.watch_interval_secs ?? 30;
              watchNext.current = Date.now() + iv * 1000;
              setWatchSecs(iv);
              setStatus(`AutoScan on: compare every ${iv}s (the hash cache means an unchanged tree costs only the walk)${currentJob.watch_auto_apply ? ' · auto-run' : ''}`);
            }}
          />
          <PathLine
            job={currentJob}
            cfgJob={cfgJob}
            busy={busy}
            selTarget={selTarget}
            pathHistory={pathHistory}
            dropOn={dropOn === 'source' || dropOn === 'target' ? dropOn : null}
            scopeRef={setPathScope}
            onCommit={(which, v) => void saveRoot(which, v)}
            onBrowse={(which) => void browseRoot(which)}
            onSwap={() => void requestSwap()}
            onSelectTarget={(i) => {
              if (busy || i === selTarget) return;
              setSelTarget(i);
              resetNavigationUi();
              const t = (currentJob?.targets ?? [])[i] ?? '';
              const restored = sessionForSelection(compareSession, currentJob, i);
              setStatus(restored
                ? `Switched target → ${t} · restored ${restored.plan.ops.length} compare items`
                : `Switched target → ${t} — Compare again (Ctrl+R)`);
            }}
            onEditGroup={(g) => { if (currentJob && !busy) setEditor({ name: currentJob.name, focusGroup: g }); }}
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
              anyCollapsed={anyCollapsed}
              pathMode={pathMode}
              onToggleFunnel={(a) => setFunnelAnchor((cur) => (cur ? null : a))}
              onToggleSame={() => {
                if (!plan) { setStatus('Compare first — identical items come from the last compare snapshot', 'err'); return; }
                setSameOpen((v) => !v);
              }}
              onExportCsv={() => void exportCsv()}
              onToggleFold={() => setCollapsedDirs(anyCollapsed ? new Set() : new Set(treeDirs))}
              // Grouping and sorting are independent now — a sort orders rows inside each group and
              // the groups among themselves, so this button no longer has to double as a sort clear
              onToggleGroup={() => {
                const next = !grouped;
                setGrouped(next);
                localStorage.setItem('sd.grouped', next ? 'on' : 'off');
                setCollapsedDirs(new Set());
              }}
              onClearSort={() => setSort(null)}
              onTogglePathMode={() => {
                const next = pathMode === 'rel' ? 'full' : 'rel';
                setPathMode(next);
                localStorage.setItem('sd.pathmode', next);
              }}
            />
          )}
          {plan && <ScanFaultBanner header={plan.header} />}
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
              ) : sameOpen && plan ? (
                <SamePanel owner={plan.owner} onClose={() => setSameOpen(false)} />
              ) : hasRows ? (
                <PlanTable
                  plan={plan!}
                  flipped={flipped}
                  checked={checked}
                  rowPlan={rowPlan}
                  displayOrder={layout.order}
                  visible={visible}
                  pathMode={pathMode}
                  grouped={grouped}
                  sort={sort}
                  collapsedDirs={collapsedDirs}
                  wrap={tableWrap}
                  resetKey={`${currentJob?.name}|${search}|${[...chips].join()}|${ovFilter}|${sort?.key}${sort?.dir}|${grouped}`}
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
          scopeRef={setEditorScope}
          apiRef={editorApi}
          busy={busy}
          onClose={() => setEditor(null)}
          onSaved={async (name, job, configRevision) => {
            setEditor(null);
            const reconciled = reconcileSavedJobSession(compareSession, name, configRevision);
            if (reconciled !== compareSession) {
              setCompareSession(reconciled);
              resetSafetyUi();
            }
            pushHistory(job.source);
            pushHistory(job.target);
            try {
              await refreshJobs(currentJob?.name === name ? name : undefined);
              if (currentJob?.name === name) setCfgJob(job);
              setStatus(`Saved '${name}'`, 'ok');
            } catch (e) {
              setStatus(`Saved '${name}', but refreshing the job list failed: ${e}`, 'err');
            }
          }}
          onDeleted={async (name) => {
            setEditor(null);
            invalidateCompareForJob(name);
            resetSafetyUi();
            if (currentJob?.name === name) { setCurrentJob(null); setSelTarget(0); }
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
