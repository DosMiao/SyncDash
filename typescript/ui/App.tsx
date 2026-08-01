// SyncDash main window.
//
// This component owns the session state (selected job, bounded compare reviews, view filters)
// and every action that crosses the Tauri boundary; everything under components/ is presentation fed by
// props. The frontend derives flipped row decisions for review, but execution receives only a
// one-use authorization token; Rust owns the authenticated plan and reconstructs every operation.

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react';
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
import type { RunRecord } from '../core/types/generated/RunRecord';

import { useStatus } from './hooks/useStatus';
import { useZoomControl } from './hooks/useZoomControl';
import {
  activeSession as sessionForSelection,
  EMPTY_COMPARE_REPOSITORY,
  invalidateJobRevision,
  invalidateSession,
  invalidateJobSession,
  ownerMatchesSelection,
  rebindSessionOwner,
  reconcileRefreshedJobSession,
  reconcileSavedJobSession,
  retainSuccessfulSession,
  successfulSession,
  snapshotJob,
  targetForSelection,
  updateSession,
} from './state/compare-session';
import type { CompareRepository, JobIdentitySnapshot } from './state/compare-session';
import {
  AutoScanTicketLedger,
  autoScanToggleAction,
  monitorOwnsAutoScanResult,
  monitorOwnsAutoScanTicket,
  reconcileAutoScanStatus,
  statusCanOwnAutoScanTrigger,
} from './state/autoscan';
import type { AutoScanStatusSource, AutoScanTicket } from './state/autoscan';
import {
  applyReviewKey,
  compareReviewKey,
  directAuthorization,
  EMPTY_APPROVAL_CHOICES,
  INITIAL_OPERATION_REVIEW,
  normalizeApprovalChoices,
  operationReviewCanSubmit,
  operationReviewPending,
  operationReviewReducer,
  ownsOperationReviewTicket,
  type ApprovalChoices,
  type OperationReviewTicket,
} from './state/operation-review';
import { RequestFence } from './state/request-fence';
import { mergeRunEventReplay } from '../core/runEvents';
import { ComparePanel } from './components/ComparePanel';
import { ScanFaultBanner } from './components/ScanFaultBanner';
import { ConfirmSheet } from './components/ConfirmSheet';
import { CompareReviewSheet } from './components/OperationReviewSheet';
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
interface CompareCompletion { plan: PlanDto }
interface AutoScanOutcome { completion: CompareCompletion | null; owned: boolean }

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
  const [compareRepository, setCompareRepository] = useState<CompareRepository>(EMPTY_COMPARE_REPOSITORY);
  const [maskHit, setMaskHit] = useState<boolean[]>([]);
  const maskRequest = useRef(new RequestFence());
  const restoreRequest = useRef(new RequestFence());
  const [busy, setBusy] = useState(false);
  const syncInFlight = useRef<OperationReviewTicket | null>(null);
  const autoApplyInFlight = useRef(false);

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
  const [confirmReviewKey, setConfirmReviewKey] = useState<string | null>(null);
  const [applyReview, dispatchApplyReview] = useReducer(operationReviewReducer, INITIAL_OPERATION_REVIEW);
  const [applyChoices, setApplyChoices] = useState<ApprovalChoices>(EMPTY_APPROVAL_CHOICES);
  const applyReviewGeneration = useRef(0);
  const applyReviewTicket = useRef<OperationReviewTicket | null>(null);
  const confirmReviewKeyRef = useRef<string | null>(null);
  const [compareReview, dispatchCompareReview] = useReducer(operationReviewReducer, INITIAL_OPERATION_REVIEW);
  const [compareChoices, setCompareChoices] = useState<ApprovalChoices>(EMPTY_APPROVAL_CHOICES);
  const compareReviewGeneration = useRef(0);
  const compareReviewTicket = useRef<OperationReviewTicket | null>(null);
  const compareReviewFetchTicket = useRef<OperationReviewTicket | null>(null);
  const compareApprovalTicket = useRef<OperationReviewTicket | null>(null);
  const [funnelAnchor, setFunnelAnchor] = useState<DOMRect | null>(null);
  const [ctx, setCtx] = useState<CtxState | null>(null);
  const [logOpen, setLogOpen] = useState(false);
  const [logReload, setLogReload] = useState(0);
  const [sameOpen, setSameOpen] = useState(false);
  const [dropOn, setDropOn] = useState<string | null>(null);
  /// The diff table's scroll container. App renders it, so App owns it and hands it down.
  const [tableWrap, setTableWrap] = useState<HTMLDivElement | null>(null);
  /// Pending root swap, held with the job it read so the confirmation can spell out both roots
  const [askSwap, setAskSwap] = useState<{
    jobId: string;
    name: string;
    job: ipc.JobFull;
    configRevision: string;
    targetIndex: number;
  } | null>(null);
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
  const cmpRunFloor = useRef(-1);
  const cmpRunReady = useRef(false);
  const compareInFlight = useRef(false);

  // AutoScan is backend-owned. The webview renders status and handles exact trigger tickets; it
  // never owns the clock or assumes that remaining mounted means the watcher is still alive.
  const [autoScanStatus, setAutoScanStatus] = useState<ipc.AutoScanStatusDto | null>(null);
  const autoScanStatusRef = useRef<ipc.AutoScanStatusDto | null>(null);
  const autoScanTicket = useRef<AutoScanTicket | null>(null);
  const autoScanLedger = useRef(new AutoScanTicketLedger<AutoScanOutcome>());
  const autoScanControlRequest = useRef(0);
  const [autoScanControlPending, setAutoScanControlPending] = useState<'start' | 'stop' | null>(null);
  const autoScanControlPendingRef = useRef<'start' | 'stop' | null>(null);
  const autoScanTriggerRef = useRef<(trigger: ipc.AutoScanTriggerDto) => Promise<void>>(async () => {});

  const editorApi = useRef<EditorApi | null>(null);
  const { status, set: setStatus, withUndo: setStatusUndo, runUndo } = useStatus('');
  const zoom = useZoomControl();
  const selectionRef = useRef<{ job: JobDto | null; targetIndex: number }>({ job: null, targetIndex: 0 });
  selectionRef.current = { job: currentJob, targetIndex: selTarget };

  // Derived view

  const activeCompare = sessionForSelection(compareRepository, currentJob, selTarget);
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
  const reviewedRows = useMemo(() => (plan ? selectedRows(final, flipped) : []), [plan, final, flipped]);
  const reviewKey = useMemo(() => (
    plan && currentJob
      ? applyReviewKey(plan.owner, currentJob.job_id, currentJob.config_revision, selTarget, reviewedRows)
      : null
  ), [plan, currentJob, selTarget, reviewedRows]);
  const currentReviewKeyRef = useRef<string | null>(reviewKey);
  currentReviewKeyRef.current = reviewKey;
  const currentCompareReviewKey = useMemo(() => (
    currentJob
      ? compareReviewKey(currentJob.job_id, currentJob.config_revision, selTarget)
      : null
  ), [currentJob, selTarget]);
  const currentCompareReviewKeyRef = useRef<string | null>(currentCompareReviewKey);
  currentCompareReviewKeyRef.current = currentCompareReviewKey;

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
      if (flipped[i]) s.flips++;
    }
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
      setCurrentJob((cur) => (
        cur?.name === keepName ? list.find((x) => x.job_id === cur.job_id) ?? null : cur
      ));
    }
    return list;
  }, []);

  const refreshLastSyncs = useCallback(() => {
    ipc.lastSyncs().then(setLastMap).catch(() => { /* missing logs are not fatal */ });
  }, []);

  const reportMutationFailure = useCallback(async (name: string, action: string, error: unknown) => {
    try {
      await refreshJobs(name);
      setStatus(`${action}: ${error} · refreshed the job registry; no unseen changes were overwritten`, 'err');
    } catch (refreshError) {
      setStatus(`${action}: ${error} · job-registry refresh failed: ${refreshError}`, 'err');
    }
  }, [refreshJobs, setStatus]);

  const resetConfirmation = useCallback(() => {
    applyReviewTicket.current = null;
    confirmReviewKeyRef.current = null;
    setApplyChoices(EMPTY_APPROVAL_CHOICES);
    setConfirmOpen(false);
    setConfirmReviewKey(null);
    dispatchApplyReview({ type: 'reset' });
  }, []);

  const resetCompareReview = useCallback(() => {
    compareReviewTicket.current = null;
    compareReviewFetchTicket.current = null;
    compareApprovalTicket.current = null;
    setCompareChoices(EMPTY_APPROVAL_CHOICES);
    dispatchCompareReview({ type: 'reset' });
  }, []);

  const resetSafetyUi = useCallback(() => {
    resetConfirmation();
    resetCompareReview();
    setSameOpen(false);
    setFunnelAnchor(null);
    setCtx(null);
    setAskSwap(null);
  }, [resetCompareReview, resetConfirmation]);

  const clearMasks = useCallback(() => {
    maskRequest.current.invalidate();
    setMaskHit([]);
  }, []);

  const acceptAutoScanStatus = useCallback((
    incoming: ipc.AutoScanStatusDto,
    source: AutoScanStatusSource,
    completedTicketId?: number,
  ) => {
    const current = autoScanStatusRef.current;
    const next = reconcileAutoScanStatus(current, incoming, source, completedTicketId);
    if (!next || next === current) return false;
    autoScanStatusRef.current = next;
    setAutoScanStatus(next);
    if (next.pending_trigger) void autoScanTriggerRef.current(next.pending_trigger);
    return true;
  }, []);

  const stopAutoScan = useCallback(() => {
    if (autoScanControlPendingRef.current !== null) return;
    const request = autoScanControlRequest.current + 1;
    autoScanControlRequest.current = request;
    autoScanControlPendingRef.current = 'stop';
    setAutoScanControlPending('stop');
    setStatus('Stopping AutoScan…');
    void ipc.stopAutoScan().then((next) => {
      if (autoScanControlRequest.current !== request) return;
      acceptAutoScanStatus(next, 'stop');
      autoScanTicket.current = null;
      setStatus('AutoScan stopped');
    }).catch((error) => {
      if (autoScanControlRequest.current === request) {
        setStatus(`AutoScan could not be stopped cleanly: ${error}`, 'err');
      }
    }).finally(() => {
      if (autoScanControlRequest.current === request) {
        autoScanControlPendingRef.current = null;
        setAutoScanControlPending(null);
      }
    });
  }, [acceptAutoScanStatus, setStatus]);

  const resetNavigationUi = useCallback(() => {
    resetSafetyUi();
    setChips(new Set());
    setSearch('');
    setOvFilter(null);
    setOvExpanded(new Set());
    setSort(null);
    setVfilter(EMPTY_FILTER);
    clearMasks();
  }, [clearMasks, resetSafetyUi]);

  const previousReviewKey = useRef<string | null>(reviewKey);
  useEffect(() => {
    if (previousReviewKey.current === reviewKey) return;
    previousReviewKey.current = reviewKey;
    if (!confirmOpen) return;
    resetConfirmation();
    setStatus('The reviewed action set changed — open confirmation again', 'err');
  }, [reviewKey, confirmOpen, resetConfirmation, setStatus]);

  const previousCompareReviewKey = useRef<string | null>(currentCompareReviewKey);
  useEffect(() => {
    if (previousCompareReviewKey.current === currentCompareReviewKey) return;
    previousCompareReviewKey.current = currentCompareReviewKey;
    resetCompareReview();
  }, [currentCompareReviewKey, resetCompareReview]);

  useEffect(() => {
    if (!currentJob || currentJob.targets.length === 0 || selTarget < currentJob.targets.length) return;
    setSelTarget(0);
    resetNavigationUi();
    setStatus(`'${currentJob.name}' no longer has target ${selTarget + 1}; selected target 1`);
  }, [currentJob, selTarget, resetNavigationUi, setStatus]);

  const invalidateCompareRevision = useCallback((jobId: string, configRevision: string) => {
    setCompareRepository((repository) => invalidateJobRevision(repository, jobId, configRevision));
  }, []);

  const invalidateCompareJob = useCallback((jobId: string) => {
    setCompareRepository((repository) => invalidateJobSession(repository, jobId));
  }, []);

  const setChecked = useCallback((next: boolean[] | ((prev: boolean[]) => boolean[])) => {
    setCompareRepository((repository) => updateSession(repository, currentJob, selTarget, (session) => ({
      ...session,
      checked: typeof next === 'function' ? next(session.checked) : next,
    })));
  }, [currentJob, selTarget]);

  const setFlipped = useCallback((next: boolean[] | ((prev: boolean[]) => boolean[])) => {
    setCompareRepository((repository) => updateSession(repository, currentJob, selTarget, (session) => ({
      ...session,
      flipped: typeof next === 'function' ? next(session.flipped) : next,
    })));
  }, [currentJob, selTarget]);

  /// A new plan means a new row set, so the mask cache is stale; the funnel criteria themselves persist
  /// (watching the same files for several rounds is common)
  const recomputeMasks = useCallback(async (p: PlanDto | null, f: boolean[], masks: string[]) => {
    if (!p || masks.length === 0) { clearMasks(); return []; }
    const owner = p.owner;
    const ticket = maskRequest.current.start(`${owner.compare_id}\0${owner.job_id}\0${owner.target_index}\0${owner.config_revision}`);
    try {
      const hits = await ipc.maskMatch(masks, p.ops.map((_, i) => eff(p, f, i).path));
      if (!maskRequest.current.owns(ticket)) return;
      const selected = selectionRef.current;
      if (!ownerMatchesSelection(owner, selected.job, selected.targetIndex)) return hits;
      setMaskHit(hits);
      return hits;
    } catch (e) {
      if (!maskRequest.current.owns(ticket)) return;
      const selected = selectionRef.current;
      if (!ownerMatchesSelection(owner, selected.job, selected.targetIndex)) return null;
      setMaskHit([]);
      setStatus(`Mask matching failed: ${e}`, 'err');
      return null;
    }
  }, [clearMasks, setStatus]);

  const requestResultRestore = useCallback((
    job: JobDto,
    targetIndex: number,
    retained: ReturnType<typeof sessionForSelection>,
    announce = true,
  ) => {
    const ticket = restoreRequest.current.start(`${job.job_id}\0${targetIndex}\0${job.config_revision}`);
    const publish = (restored: PlanDto | null) => {
      if (!restoreRequest.current.owns(ticket) || !restored) return;
      const selected = selectionRef.current;
      if (!ownerMatchesSelection(restored.owner, selected.job, selected.targetIndex)) return;
      const session = successfulSession(
        restored,
        restored.ops.map((op) => selectable(op)),
        restored.ops.map(() => false),
      );
      setCompareRepository((repository) => retainSuccessfulSession(repository, session));
      if (announce) setStatus(`${job.name} · restored ${restored.ops.length} compare items`);
    };
    const failed = (error: unknown) => {
      if (!restoreRequest.current.owns(ticket)) return;
      if (announce) setStatus(`Could not restore '${job.name}' result: ${error}`, 'err');
    };
    if (!retained) {
      void ipc.restoreCompare(job.job_id, targetIndex).then(publish).catch(failed);
      return;
    }
    void ipc.touchCompare(retained.plan.owner).then(async (backendOwner) => {
      if (!restoreRequest.current.owns(ticket)) return;
      if (!backendOwner) {
        setCompareRepository((repository) => invalidateSession(repository, retained.plan.owner));
        if (announce) setStatus(`${job.name} · retained result expired — Compare again`, 'err');
        return;
      }
      if (backendOwner.compare_id === retained.plan.owner.compare_id) {
        setCompareRepository((repository) => rebindSessionOwner(
          retainSuccessfulSession(repository, retained),
          retained.plan.owner,
          backendOwner,
        ));
        return;
      }
      publish(await ipc.restoreCompare(job.job_id, targetIndex));
    }).catch(failed);
  }, [setStatus]);

  // Actions

  const runAuthorizedCompare = useCallback(async (
    authorizationToken: string,
    comparedJob: JobIdentitySnapshot,
    targetIndex: number,
    autoTicket?: AutoScanTicket,
  ): Promise<CompareCompletion | null> => {
    if (busy || editor || compareInFlight.current || autoApplyInFlight.current) return null;
    if (autoTicket && (
      !monitorOwnsAutoScanTicket(autoScanStatusRef.current, autoScanTicket.current, autoTicket)
      || syncInFlight.current !== null
      || applyReviewTicket.current !== null
      || compareReviewTicket.current !== null
      || confirmOpen
      || operationReviewPending(compareReview)
      || operationReviewPending(applyReview)
    )) return null;
    if (!autoTicket) autoScanTicket.current = null;
    const selectedAtStart = selectionRef.current;
    const showProgress = !autoTicket || (
      selectedAtStart.job?.job_id === autoTicket.jobId
      && selectedAtStart.job.config_revision === autoTicket.configRevision
      && selectedAtStart.targetIndex === autoTicket.targetIndex
    );
    if (!autoTicket) restoreRequest.current.invalidate();
    compareInFlight.current = true;
    const name = comparedJob.name;
    if (!autoTicket) resetSafetyUi();
    setBusy(true);
    setStatus(`${autoTicket ? 'AutoScan is comparing' : 'Comparing'} '${name}'…`);
    if (showProgress) setCmpStages([]);
    cmpRate.current.clear();
    if (showProgress) setCmpCancelling(false);
    cmpRunFloor.current = cmpRunId.current;
    cmpRunReady.current = false;
    if (showProgress) setCmpActive(true);
    try {
      const p = await ipc.compareJob(authorizationToken);
      const f = p.ops.map(() => false);
      setCompareRepository((repository) => retainSuccessfulSession(
        repository,
        successfulSession(p, p.ops.map((op) => selectable(op)), f),
      ));
      if (!autoTicket) {
        setChips(new Set());
        setOvFilter(null);
        setOvExpanded(new Set());
        setSort(null);
      }
      // A job file can be edited outside the app while it is open. Compare used the authoritative
      // file, so refresh the list row before deciding whether the returned owner belongs on screen.
      // Snapshot first: refreshJobs may commit the new row (or null) before this continuation runs,
      // and then the ref no longer tells us that the selected job changed underneath this compare.
      const selectedBeforeRefresh = selectionRef.current;
      let refreshedJob: JobDto | null = null;
      let refreshProblem: unknown = null;
      try {
        const list = await refreshJobs(name);
        refreshedJob = list.find((job) => job.job_id === p.owner.job_id) ?? null;
        setCompareRepository((repository) => reconcileRefreshedJobSession(
          repository,
          { jobId: p.owner.job_id, name: p.owner.job_name, configRevision: p.owner.config_revision },
          refreshedJob,
        ));
      } catch (e) {
        refreshProblem = e;
      }
      const selected = selectionRef.current;
      const selectedJob = selected.job?.job_id === p.owner.job_id && !refreshProblem ? refreshedJob : selected.job;
      let navigationWasReset = false;
      if (selectedBeforeRefresh.job?.job_id === p.owner.job_id && !refreshProblem) {
        if (!refreshedJob) {
          setSelTarget(0);
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
      } else if (autoTicket) {
        setStatus(
          p.ops.length === 0
            ? `AutoScan finished for '${name}' — both sides are identical; result retained`
            : `AutoScan finished for '${name}' — ${p.ops.length} differences retained for review`,
          p.header.conflict_count > 0 ? 'err' : '',
        );
      } else {
        setStatus(`Compare finished for '${name}', but its job or target changed — the result was not attached to the current view`, 'err');
      }
      return { plan: p };
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
        const refreshedJob = list.find((job) => job.job_id === comparedJob.jobId) ?? null;
        setCompareRepository((repository) => reconcileRefreshedJobSession(repository, comparedJob, refreshedJob));
        if (selected.job?.job_id === comparedJob.jobId) {
          if (!refreshedJob) {
            setSelTarget(0);
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
      const base = cancelled ? 'Compare cancelled' : `${autoTicket ? 'AutoScan Compare' : 'Compare'} failed: ${e}`;
      if (refreshProblem) suffix = ` · job-list refresh failed: ${refreshProblem}`;
      setStatus(`${base}${suffix}`, cancelled && !refreshProblem ? '' : 'err');
      return null;
    } finally {
      if (showProgress) setCmpActive(false);
      setBusy(false);
      cmpRunReady.current = false;
      compareInFlight.current = false;
    }
  }, [busy, editor, confirmOpen, compareReview, applyReview, vfilter.masks, recomputeMasks, refreshJobs, resetNavigationUi, resetSafetyUi, setStatus]);

  const doCompare = useCallback(async (autoTicket?: AutoScanTicket): Promise<CompareCompletion | null> => {
    if (busy || editor || compareInFlight.current || syncInFlight.current || autoApplyInFlight.current) return null;
    if (!autoTicket && !currentJob) return null;
    if (!autoTicket && applyReviewTicket.current) return null;
    if (!autoTicket && operationReviewPending(compareReview)) return null;
    if (autoTicket && (
      !monitorOwnsAutoScanTicket(autoScanStatusRef.current, autoScanTicket.current, autoTicket)
      || applyReviewTicket.current !== null
      || compareReviewTicket.current !== null
      || confirmOpen
      || operationReviewPending(compareReview)
      || operationReviewPending(applyReview)
    )) return null;

    const comparedJob: JobIdentitySnapshot = autoTicket
      ? { jobId: autoTicket.jobId, name: autoTicket.jobName, configRevision: autoTicket.configRevision }
      : snapshotJob(currentJob!);
    const targetIndex = autoTicket?.targetIndex ?? selTarget;
    const key = compareReviewKey(comparedJob.jobId, comparedJob.configRevision, targetIndex);

    if (autoTicket) {
      setStatus(`AutoScan is reviewing Compare authorization for '${comparedJob.name}'…`);
      try {
        const review = await ipc.reviewCompare(comparedJob.jobId, targetIndex);
        const stillOwned = monitorOwnsAutoScanTicket(
          autoScanStatusRef.current,
          autoScanTicket.current,
          autoTicket,
        );
        if (!stillOwned) return null;
        const authorization = directAuthorization(review);
        if (!authorization) {
          setStatus(
            review.status === 'direct_authorized'
              ? `AutoScan paused: the direct Compare review for '${comparedJob.name}' was internally inconsistent`
              : `AutoScan paused: Compare requires an exact interactive authorization for '${comparedJob.name}'`,
            'err',
          );
          return null;
        }
        return runAuthorizedCompare(
          authorization.authorization_token,
          comparedJob,
          targetIndex,
          autoTicket,
        );
      } catch (error) {
        setStatus(`AutoScan could not review Compare authorization: ${error}`, 'err');
        return null;
      }
    }

    if (compareReviewFetchTicket.current?.key === key) return null;

    const ticket: OperationReviewTicket = {
      key,
      generation: compareReviewGeneration.current + 1,
    };
    compareReviewGeneration.current = ticket.generation;
    compareReviewTicket.current = ticket;
    compareReviewFetchTicket.current = ticket;
    setCompareChoices(EMPTY_APPROVAL_CHOICES);
    dispatchCompareReview({ type: 'begin', ticket });
    setStatus(`Reviewing Compare authorization for '${comparedJob.name}'…`);
    try {
      const review = await ipc.reviewCompare(comparedJob.jobId, targetIndex);
      if (!ownsOperationReviewTicket(compareReviewTicket.current, ticket, currentCompareReviewKeyRef.current)) {
        return null;
      }
      dispatchCompareReview({ type: 'resolved', ticket, review });
      const authorization = directAuthorization(review);
      if (authorization) {
        return runAuthorizedCompare(
          authorization.authorization_token,
          comparedJob,
          targetIndex,
        );
      }
      if (review.status === 'direct_authorized') {
        const error = 'The direct Compare review was internally inconsistent and was rejected';
        dispatchCompareReview({ type: 'failed', ticket, error });
        setStatus(error, 'err');
        return null;
      }
      if (review.status === 'blocked') {
        setStatus(`Compare is blocked for '${comparedJob.name}' — review the required fixes`, 'err');
      } else if (review.status === 'confirmation_required') {
        setStatus(`Compare requires your approval for '${comparedJob.name}'`);
      } else {
        const error = 'The Compare review returned an unknown status and was rejected';
        dispatchCompareReview({ type: 'failed', ticket, error });
        setStatus(error, 'err');
      }
      return null;
    } catch (error) {
      if (!ownsOperationReviewTicket(compareReviewTicket.current, ticket, currentCompareReviewKeyRef.current)) {
        return null;
      }
      dispatchCompareReview({ type: 'failed', ticket, error: String(error) });
      setStatus(`Compare authorization review failed: ${error}`, 'err');
      return null;
    } finally {
      if (compareReviewFetchTicket.current?.generation === ticket.generation
        && compareReviewFetchTicket.current.key === ticket.key) {
        compareReviewFetchTicket.current = null;
      }
    }
  }, [currentJob, busy, editor, compareReview, applyReview, confirmOpen, selTarget, runAuthorizedCompare, setStatus]);

  const approveCompareReview = useCallback(async () => {
    const ticket = compareReviewTicket.current;
    const review = compareReview.review;
    if (!ticket || !review || !operationReviewCanSubmit(compareReview, compareChoices)) return;
    if (review.status !== 'confirmation_required' || !review.challenge_id) return;
    if (compareApprovalTicket.current?.generation === ticket.generation
      && compareApprovalTicket.current.key === ticket.key) return;
    compareApprovalTicket.current = ticket;
    const choices = normalizeApprovalChoices(review, compareChoices);
    dispatchCompareReview({ type: 'begin_approval', ticket });
    setStatus('Authorizing this exact Compare operation…');
    try {
      const authorization = await ipc.approveOperation(
        review.challenge_id,
        choices.acknowledgeHealth,
        choices.acceptCapabilities,
        choices.rememberForSession,
        choices.allowUnattended,
      );
      if (!ownsOperationReviewTicket(compareReviewTicket.current, ticket, currentCompareReviewKeyRef.current)) return;
      const selected = selectionRef.current;
      if (!selected.job) return;
      dispatchCompareReview({ type: 'authorized', ticket, authorization });
      await runAuthorizedCompare(
        authorization.authorization_token,
        snapshotJob(selected.job),
        selected.targetIndex,
      );
    } catch (error) {
      if (!ownsOperationReviewTicket(compareReviewTicket.current, ticket, currentCompareReviewKeyRef.current)) return;
      dispatchCompareReview({ type: 'approval_failed', ticket, error: String(error) });
      setStatus(`Compare authorization failed: ${error}`, 'err');
    } finally {
      if (compareApprovalTicket.current?.generation === ticket.generation
        && compareApprovalTicket.current.key === ticket.key) {
        compareApprovalTicket.current = null;
      }
    }
  }, [compareChoices, compareReview, runAuthorizedCompare, setStatus]);

  const openConfirm = useCallback(async () => {
    if (!currentJob
      || !plan
      || !reviewKey
      || busy
      || autoApplyInFlight.current
      || operationReviewPending(applyReview)
      || applyReviewTicket.current
      || compareReviewFetchTicket.current
      || compareReview.review) return;
    const hiddenChecked = checked.filter(Boolean).length - final.length;
    if (final.length === 0) {
      setStatus(
        hiddenChecked > 0 ? 'Every checked row is hidden by a filter — clear the filter first' : 'Nothing is checked',
        'err',
      );
      return;
    }
    const ticket: OperationReviewTicket = {
      key: reviewKey,
      generation: applyReviewGeneration.current + 1,
    };
    applyReviewGeneration.current = ticket.generation;
    applyReviewTicket.current = ticket;
    confirmReviewKeyRef.current = reviewKey;
    setConfirmReviewKey(reviewKey);
    setApplyChoices(EMPTY_APPROVAL_CHOICES);
    dispatchApplyReview({ type: 'begin', ticket });
    setConfirmOpen(true);
    try {
      const review = await ipc.reviewApply(plan.owner, reviewedRows);
      if (!ownsOperationReviewTicket(applyReviewTicket.current, ticket, currentReviewKeyRef.current)) return;
      dispatchApplyReview({ type: 'resolved', ticket, review });
    } catch (error) {
      if (!ownsOperationReviewTicket(applyReviewTicket.current, ticket, currentReviewKeyRef.current)) return;
      dispatchApplyReview({ type: 'failed', ticket, error: String(error) });
    }
  }, [currentJob, plan, reviewKey, busy, applyReview, compareReview.review, checked, final, reviewedRows, setStatus]);

  const doSync = useCallback(async () => {
    if (
      !currentJob
      || !plan
      || !reviewKey
      || !confirmOpen
      || confirmReviewKey !== reviewKey
      || confirmReviewKeyRef.current !== reviewKey
      || !operationReviewCanSubmit(applyReview, applyChoices)
    ) {
      setStatus('Apply is unavailable until this exact reviewed action set is authorized', 'err');
      return;
    }
    const ticket = applyReviewTicket.current;
    const review = applyReview.review;
    if (!ticket || !review) return;
    if (busy || autoApplyInFlight.current || (syncInFlight.current?.generation === ticket.generation
      && syncInFlight.current.key === ticket.key)) return;
    syncInFlight.current = ticket;
    const selected = reviewedRows;
    const applyingJob = currentJob;
    // Whether the progress window stays during a sync is its own Auto-close / When-finished business
    let launchId: number | null = null;
    let executionStarted = false;
    try {
      let authorization = review.authorization;
      if (review.status === 'confirmation_required') {
        if (!review.challenge_id) throw new Error('the safety review did not provide an approval challenge');
        const choices = normalizeApprovalChoices(review, applyChoices);
        dispatchApplyReview({ type: 'begin_approval', ticket });
        setStatus('Authorizing this exact apply operation…');
        try {
          authorization = await ipc.approveOperation(
            review.challenge_id,
            choices.acknowledgeHealth,
            choices.acceptCapabilities,
            choices.rememberForSession,
            choices.allowUnattended,
          );
        } catch (error) {
          if (ownsOperationReviewTicket(applyReviewTicket.current, ticket, currentReviewKeyRef.current)) {
            dispatchApplyReview({ type: 'approval_failed', ticket, error: String(error) });
            setStatus(`Apply authorization failed: ${error}`, 'err');
          }
          return;
        }
        if (!ownsOperationReviewTicket(applyReviewTicket.current, ticket, currentReviewKeyRef.current)) return;
        dispatchApplyReview({ type: 'authorized', ticket, authorization });
      }
      if (!authorization) throw new Error('the safety review did not authorize this operation');

      resetConfirmation();
      setBusy(true);
      setStatus(`Synchronizing '${applyingJob.name}' (${selected.length} items)...`);
      // The command returns only after the new window has installed its run-progress listener.
      // Starting apply any earlier loses the phase start/totals on a freshly opened window.
      launchId = await ipc.openProgressWindow();
      // Once apply is invoked, any rejection may still follow partial writes. Retire this plan before
      // crossing that boundary; only the post-run compare is entitled to publish another one.
      invalidateCompareRevision(applyingJob.job_id, applyingJob.config_revision);
      resetSafetyUi();
      executionStarted = true;
      const r = await ipc.applyJob(authorization.authorization_token, launchId);
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
      setStatus(
        executionStarted
          ? `Apply failed and may have made partial changes: ${e} — Compare again before continuing`
          : `Apply did not start: ${e} — the reviewed result was retained`,
        'err',
      );
      setBusy(false);
      requestResultRestore(applyingJob, selTarget, activeCompare, false);
    } finally {
      if (launchId !== null) void ipc.cancelProgressLaunch(launchId);
      if (syncInFlight.current?.generation === ticket.generation
        && syncInFlight.current.key === ticket.key) {
        syncInFlight.current = null;
      }
    }
  }, [currentJob, activeCompare, plan, reviewKey, confirmOpen, confirmReviewKey, applyReview, applyChoices, busy, reviewedRows, selTarget, doCompare, refreshLastSyncs, invalidateCompareRevision, requestResultRestore, resetConfirmation, resetSafetyUi, setStatus]);

  const selectJob = useCallback((j: JobDto) => {
    if (currentJob?.job_id === j.job_id) return;
    const targetIndex = targetForSelection(compareRepository, j);
    const restored = sessionForSelection(compareRepository, j, targetIndex);
    selectionRef.current = { job: j, targetIndex };
    setCurrentJob(j);
    setSelTarget(targetIndex);
    resetNavigationUi();
    setStatus(restored
      ? `${j.name} · restored ${restored.plan.ops.length} compare items`
      : `${j.name} · ${j.mode}${j.rigor !== 'standard' ? ` · ${j.rigor}` : ''}`);
    requestResultRestore(j, targetIndex, restored);
  }, [currentJob?.job_id, compareRepository, requestResultRestore, resetNavigationUi, setStatus]);

  /// Write a root edited on the main screen back to the job TOML. Changing a root invalidates the current
  /// plan, so clear it too. For multi-target jobs, only the currently selected target changes.
  const saveRoot = useCallback(async (which: 'source' | 'target', value: string) => {
    if (!currentJob) return;
    const name = currentJob.name;
    const v = value.trim();
    if (!v) return;
    let detail: ipc.JobDetailDto;
    try {
      detail = await ipc.getJob(name);
    } catch (e) {
      await reportMutationFailure(name, `Failed to read the job before changing ${which}`, e);
      return;
    }
    const j = detail.job;
    const hasTargetList = j.targets.length > 0;
    const ts = hasTargetList ? [...j.targets] : [j.target];
    const before = which === 'source' ? j.source : ts[selTarget];
    if (before === v) return; // unchanged means no disk write — otherwise every blur would bump the mtime
    const next: ipc.JobFull = which === 'source'
      ? { ...j, source: v }
      : {
        ...j,
        target: selTarget === 0 ? v : j.target,
        targets: hasTargetList ? ts.map((t, i) => (i === selTarget ? v : t)) : [],
      };
    let saved: ipc.JobSaveDto;
    try {
      saved = await ipc.saveJob(name, next, {
        originalName: detail.name,
        expectedRevision: detail.config_revision,
      });
    } catch (e) {
      await reportMutationFailure(name, `Failed to write ${which} back to the job`, e);
      return;
    }
    // The mutation is committed at this point. Retire its old compare immediately, and never let a
    // later list-refresh failure make the UI claim that this successful disk write failed.
    setCompareRepository((repository) => reconcileSavedJobSession(
      repository,
      saved,
      { jobId: detail.job_id, name: detail.name, configRevision: detail.config_revision },
    ));
    resetSafetyUi();
    pushHistory(v);
    const undo = async () => {
      const back: ipc.JobFull = which === 'source'
        ? { ...next, source: before }
        : { ...next, target: selTarget === 0 ? before : next.target, targets: next.targets.map((t, i) => (i === selTarget ? before : t)) };
      try {
        const restored = await ipc.saveJob(saved.name, back, {
          originalName: saved.name,
          expectedRevision: saved.config_revision,
        });
        setCompareRepository((repository) => reconcileSavedJobSession(
          repository,
          restored,
          { jobId: saved.job_id, name: saved.name, configRevision: saved.config_revision },
        ));
      } catch (e) {
        await reportMutationFailure(saved.name, `Could not restore ${which}`, e);
        return;
      }
      resetSafetyUi();
      try {
        await refreshJobs(saved.name);
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
  }, [currentJob, selTarget, pushHistory, refreshJobs, reportMutationFailure, resetSafetyUi, setStatus, setStatusUndo]);

  /// The FFS ⇄ swaps the in-memory config; our jobs are named TOML files on disk, so a swap has to hit
  /// the disk — otherwise the two roots in the plan header say something different from the job file, and
  /// both run logs and archive refresh point in the wrong direction. Read the job first so the
  /// confirmation can name the two roots it is about to exchange.
  const requestSwap = useCallback(async () => {
    if (!currentJob || busy) return;
    try {
      const detail = await ipc.getJob(currentJob.name);
      const job = detail.job;
      const targets = job.targets.length ? job.targets : [job.target];
      if (selTarget >= targets.length) {
        setStatus(`Target ${selTarget + 1} no longer exists — refresh the job before swapping`, 'err');
        return;
      }
      setAskSwap({
        jobId: detail.job_id,
        name: detail.name,
        job,
        configRevision: detail.config_revision,
        targetIndex: selTarget,
      });
    } catch (e) {
      await reportMutationFailure(currentJob.name, 'Failed to read job before swapping', e);
    }
  }, [currentJob, busy, selTarget, reportMutationFailure, setStatus]);

  const doSwap = useCallback(async (
    jobId: string,
    name: string,
    j: ipc.JobFull,
    configRevision: string,
    targetIndex: number,
  ) => {
    const targets = j.targets.length ? [...j.targets] : [j.target];
    const hasTargetList = j.targets.length > 0;
    const selectedTarget = targets[targetIndex];
    if (selectedTarget === undefined) {
      setStatus(`Swap failed: target ${targetIndex + 1} no longer exists`, 'err');
      return;
    }
    const nextTargets = targets.map((target, index) => (index === targetIndex ? j.source : target));
    const next: ipc.JobFull = {
      ...j,
      source: selectedTarget,
      target: targetIndex === 0 ? j.source : j.target,
      targets: hasTargetList ? nextTargets : [],
    };
    let saved: ipc.JobSaveDto;
    try {
      saved = await ipc.saveJob(name, next, {
        originalName: name,
        expectedRevision: configRevision,
      });
    } catch (e) {
      await reportMutationFailure(name, 'Swap failed', e);
      return;
    }
    setCompareRepository((repository) => reconcileSavedJobSession(
      repository,
      saved,
      { jobId, name, configRevision },
    ));
    resetSafetyUi();
    setChips(new Set());
    setOvFilter(null);
    setOvExpanded(new Set());
    const undo = async () => {
      try {
        const restored = await ipc.saveJob(saved.name, j, {
          originalName: saved.name,
          expectedRevision: saved.config_revision,
        });
        setCompareRepository((repository) => reconcileSavedJobSession(
          repository,
          restored,
          { jobId: saved.job_id, name: saved.name, configRevision: saved.config_revision },
        ));
      } catch (e) {
        await reportMutationFailure(saved.name, 'Could not undo the root swap', e);
        return;
      }
      resetSafetyUi();
      try {
        await refreshJobs(saved.name);
        setStatus(`Restored the two roots of '${saved.name}'`);
      } catch (e) {
        setStatus(`Restored the two roots of '${saved.name}', but refreshing the job list failed: ${e}`, 'err');
      }
    };
    const success = `Swapped the two roots of '${name}' — Compare again (Ctrl+R)`;
    try {
      await refreshJobs(name);
      setStatusUndo(success, 'Undo swap', undo);
    } catch (e) {
      setStatusUndo(`${success}; refreshing the job list failed: ${e}`, 'Undo swap', undo, 'err');
    }
  }, [refreshJobs, reportMutationFailure, resetSafetyUi, setStatus, setStatusUndo]);

  /// Write an exclude back into the job's exclude list. Pruning during the scan only takes effect at the
  /// next Compare, so the message has to say so and leave an undo behind.
  const addExcludes = useCallback(async (masks: string[], label: string) => {
    if (!currentJob) { setStatus('Select a job first', 'err'); return; }
    const name = currentJob.name;
    let detail: ipc.JobDetailDto;
    try {
      detail = await ipc.getJob(name);
    } catch (e) {
      await reportMutationFailure(name, 'Failed to read the job before adding the exclude', e);
      return;
    }
    const j = detail.job;
    const prev = [...j.exclude];
    // Folded the way the engine folds, not by string equality: a mask typed with backslashes, in a
    // different case, or in NFD is the same rule to the filter, and appending it again would leave
    // two lines that mean one thing — and a preset box unticked next to its own pattern
    const { next, added: add } = addExcludeEntries(prev, masks);
    if (!add.length) { setStatus(`The job already has ${masks.length > 1 ? 'all of these masks' : 'this exclude'}`); return; }
    const nextJob = { ...j, exclude: next };
    let saved: ipc.JobSaveDto;
    try {
      saved = await ipc.saveJob(name, nextJob, {
        originalName: detail.name,
        expectedRevision: detail.config_revision,
      });
    } catch (e) {
      await reportMutationFailure(name, 'Failed to write exclude', e);
      return;
    }
    setCompareRepository((repository) => reconcileSavedJobSession(
      repository,
      saved,
      { jobId: detail.job_id, name: detail.name, configRevision: detail.config_revision },
    ));
    resetSafetyUi();
    const undo = async () => {
      try {
        const restored = await ipc.saveJob(saved.name, j, {
          originalName: saved.name,
          expectedRevision: saved.config_revision,
        });
        setCompareRepository((repository) => reconcileSavedJobSession(
          repository,
          restored,
          { jobId: saved.job_id, name: saved.name, configRevision: saved.config_revision },
        ));
      } catch (e) {
        await reportMutationFailure(saved.name, 'Could not undo the exclude', e);
        return;
      }
      resetSafetyUi();
      try {
        await refreshJobs(saved.name);
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
  }, [currentJob, refreshJobs, reportMutationFailure, resetSafetyUi, setStatus, setStatusUndo]);

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
        defaultPath: which === 'source'
          ? currentJob?.source
          : currentJob?.targets[selTarget] ?? currentJob?.target,
      });
      if (p) await saveRoot(which, p);
    } catch (e) {
      setStatus(`Can't open the picker: ${e}`, 'err');
    }
  }, [currentJob, selTarget, saveRoot, setStatus]);

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
    let disposed = false;
    let dispose: (() => void) | undefined;
    let ready = false;
    let lastSequence = 0;
    const queued: CompareProgressEvent[] = [];
    const handle = (e: CompareProgressEvent) => {
      if (e.purpose !== 'compare') return;
      if (!cmpRunReady.current && e.run_id <= cmpRunFloor.current) return;
      if (e.run_id < cmpRunId.current) return;
      if (e.run_id > cmpRunId.current) {
        cmpRunId.current = e.run_id;
        cmpRate.current.clear();
        setCmpStages([]);
      }
      cmpRunReady.current = true;
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
    };
    const publish = (event: CompareProgressEvent) => {
      if (!ready) {
        queued.push(event);
        return;
      }
      if (event.sequence <= lastSequence) return;
      lastSequence = event.sequence;
      handle(event);
    };
    void listen<CompareProgressEvent>('run-progress', (event) => publish(event.payload))
      .then(async (unlisten) => {
        if (disposed) {
          unlisten();
          return;
        }
        dispose = unlisten;
        let replay: CompareProgressEvent[] = [];
        try {
          replay = await ipc.replayRunEvents('compare');
        } catch (error) {
          setStatus(`Could not restore compare progress after reconnect: ${error}`, 'err');
        }
        if (disposed) return;
        const pending = mergeRunEventReplay(replay, queued);
        queued.length = 0;
        ready = true;
        for (const event of pending) publish(event);
      })
      .catch((error) => {
        if (!disposed) setStatus(`Could not subscribe to compare progress: ${error}`, 'err');
      });
    return () => {
      disposed = true;
      dispose?.();
    };
  }, [setStatus]);

  useEffect(() => {
    const un = listen<string>('main-close-blocked', (event) => {
      setStatus(event.payload, 'err');
    });
    return () => { un.then((dispose) => dispose()); };
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
          if (info.source.readiness === 'not_directory') {
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
    ipc.getJob(currentJob.name).then((detail) => {
      if (live) setCfgJob(detail.job);
    }).catch((error) => {
      if (!live) return;
      setCfgJob(null);
      setStatus(`Failed to load '${currentJob.name}' settings: ${error}`, 'err');
    });
    return () => { live = false; };
  }, [currentJob, setStatus]);

  autoScanTriggerRef.current = async (trigger) => {
    const ticket: AutoScanTicket = {
      generation: trigger.generation,
      ticketId: trigger.ticket_id,
      jobId: trigger.job_id,
      jobName: trigger.job_name,
      configRevision: trigger.config_revision,
      targetIndex: trigger.target_index,
      autoApply: trigger.auto_apply,
    };
    const claim = autoScanLedger.current.claim(ticket);
    if (claim.kind === 'duplicate') return;
    if (claim.kind === 'capacity') {
      setStatus('AutoScan rejected a trigger because its bounded recovery ledger is full', 'err');
      try {
        acceptAutoScanStatus(
          await ipc.completeAutoScan(ticket.generation, ticket.ticketId, false, null),
          'completion',
          ticket.ticketId,
        );
      } catch (error) {
        setStatus(`AutoScan could not release the rejected trigger: ${error}`, 'err');
      }
      return;
    }

    const observed = autoScanStatusRef.current;
    if (observed?.active
      && observed.generation === ticket.generation
      && observed.job_id === ticket.jobId
      && observed.config_revision === ticket.configRevision
      && observed.target_index === ticket.targetIndex) {
      // The trigger event is itself the newest backend cursor. Materialize it into status so a
      // delayed completion for N cannot race an event for N+1 merely because no status event exists.
      acceptAutoScanStatus({
        ...observed,
        latest_ticket_id: ticket.ticketId,
        active_ticket: ticket.ticketId,
        pending_trigger: trigger,
        mode: trigger.mode,
      }, 'event');
    }

    let outcome: AutoScanOutcome;
    if (claim.kind === 'retry_completion') {
      outcome = claim.outcome;
    } else {
      let monitor = autoScanStatusRef.current;
      const triggerMatchesMonitor = statusCanOwnAutoScanTrigger(monitor, ticket);
      if (!triggerMatchesMonitor) {
        try {
          acceptAutoScanStatus(await ipc.autoScanStatus(), 'snapshot');
          monitor = autoScanStatusRef.current;
        } catch (error) {
          setStatus(`AutoScan could not verify recovered trigger ownership: ${error}`, 'err');
        }
      }

      const monitorMatches = statusCanOwnAutoScanTrigger(monitor, ticket);
      let completion: CompareCompletion | null = null;
      if (monitorMatches) {
        autoScanTicket.current = ticket;
        completion = await doCompare(ticket);
      }
      const owned = completion !== null && monitorOwnsAutoScanResult(
        autoScanStatusRef.current,
        autoScanTicket.current,
        ticket,
        completion.plan.owner,
      );
      outcome = { completion, owned };
      if (!autoScanLedger.current.prepareCompletion(ticket, outcome)) return;
    }

    let completionCurrent = false;
    try {
      const next = await ipc.completeAutoScan(
        ticket.generation,
        ticket.ticketId,
        outcome.owned,
        outcome.owned && outcome.completion ? outcome.completion.plan.owner : null,
      );
      acceptAutoScanStatus(next, 'completion', ticket.ticketId);
      autoScanLedger.current.completed(ticket);
      const current = autoScanStatusRef.current;
      completionCurrent = current?.active === true
        && current.generation === ticket.generation
        && current.latest_ticket_id === ticket.ticketId
        && current.job_id === ticket.jobId
        && current.config_revision === ticket.configRevision
        && current.target_index === ticket.targetIndex
        && current.active_ticket === null
        && current.pending_trigger === null;
    } catch (error) {
      autoScanLedger.current.completionFailed(ticket);
      try {
        const recovered = await ipc.autoScanStatus();
        const completedDespiteLostResponse = recovered.active
          && recovered.generation === ticket.generation
          && recovered.latest_ticket_id === ticket.ticketId
          && recovered.job_id === ticket.jobId
          && recovered.config_revision === ticket.configRevision
          && recovered.target_index === ticket.targetIndex
          && recovered.active_ticket === null
          && recovered.pending_trigger === null;
        if (completedDespiteLostResponse) {
          acceptAutoScanStatus(recovered, 'completion', ticket.ticketId);
          autoScanLedger.current.completed(ticket);
          completionCurrent = true;
        } else {
          const samePending = recovered.active
            && recovered.generation === ticket.generation
            && recovered.pending_trigger?.ticket_id === ticket.ticketId;
          if (samePending) {
            // Connectivity is back and the backend proves the success was not committed. Release
            // this cycle as failed instead of leaving the worker waiting forever or rerunning Compare.
            try {
              const released = await ipc.completeAutoScan(ticket.generation, ticket.ticketId, false, null);
              acceptAutoScanStatus(released, 'completion', ticket.ticketId);
              autoScanLedger.current.completed(ticket);
              if (autoScanTicket.current?.generation === ticket.generation
                && autoScanTicket.current.ticketId === ticket.ticketId) autoScanTicket.current = null;
              setStatus(`AutoScan deferred this cycle after completion recovery failed: ${error}`, 'err');
            } catch (releaseError) {
              setStatus(`AutoScan could not release its recovered ticket: ${releaseError}`, 'err');
            }
            return;
          } else {
            acceptAutoScanStatus(recovered, 'snapshot');
            autoScanLedger.current.completed(ticket);
          }
        }
      } catch {
        // Preserve the ready ledger record. Recovery can retry this completion without rescanning.
      }
      if (!completionCurrent) {
        const current = autoScanStatusRef.current;
        const stillPending = current?.active === true
          && current.generation === ticket.generation
          && current.pending_trigger?.ticket_id === ticket.ticketId;
        if (!stillPending
          && autoScanTicket.current?.generation === ticket.generation
          && autoScanTicket.current.ticketId === ticket.ticketId) autoScanTicket.current = null;
        if (autoScanTicket.current?.generation === ticket.generation
          && autoScanTicket.current.ticketId === ticket.ticketId) {
          setStatus(`AutoScan could not commit its compare ticket: ${error}`, 'err');
        }
        return;
      }
    }
    if (autoScanTicket.current?.generation === ticket.generation
      && autoScanTicket.current.ticketId === ticket.ticketId) autoScanTicket.current = null;
    if (!completionCurrent) return;
    if (!outcome.owned || !outcome.completion) return;

    const freshPlan = outcome.completion.plan;
    if (freshPlan.ops.length === 0) return;
    if (!ticket.autoApply) {
      setStatus(`AutoScan found ${freshPlan.ops.length} differences — review required`, 'err');
      return;
    }

    if (compareInFlight.current || syncInFlight.current || editor || applyReviewTicket.current || compareReviewTicket.current) {
      setStatus(`AutoScan found ${freshPlan.ops.length} differences — another interaction owns execution; review required`, 'err');
      return;
    }
    const applyStatus = autoScanStatusRef.current;
    if (applyStatus?.active !== true
      || applyStatus.generation !== ticket.generation
      || applyStatus.latest_ticket_id !== ticket.ticketId
      || applyStatus.job_id !== ticket.jobId
      || applyStatus.config_revision !== ticket.configRevision
      || applyStatus.target_index !== ticket.targetIndex
      || applyStatus.active_ticket !== null
      || applyStatus.pending_trigger !== null
      || autoScanTicket.current !== null) return;
    setStatus(`AutoScan found ${freshPlan.ops.length} differences — checking the backend-owned AutoApply ticket…`);
    autoApplyInFlight.current = true;
    setBusy(true);
    try {
      let authorization: ipc.AuthorizationDto;
      try {
        authorization = await ipc.authorizeAutoScanApply(ticket.generation, ticket.ticketId);
      } catch (error) {
        setStatus(
          `AutoApply did not run: interactive review is required for this exact job revision, target, and capability set: ${error}`,
          'err',
        );
        return;
      }
      // Authorization failure leaves the freshly compared result intact for interactive review.
      // Once execution begins, retire it because a rejected apply may still have made partial writes.
      invalidateCompareRevision(ticket.jobId, ticket.configRevision);
      try {
        const result = await ipc.applyJob(authorization.authorization_token);
        refreshLastSyncs();
        setLogReload((value) => value + 1);
        setStatus(
          result.cancelled
            ? `Auto-sync stopped after ${result.done} actions`
            : `Auto-sync finished: ${result.done} run, ${result.skipped} skipped, ${result.errors} errors`,
          result.errors ? 'err' : 'ok',
        );
      } catch (error) {
        setStatus(
          `The authorized auto-sync failed and may have made partial changes: ${error} — Compare again before continuing`,
          'err',
        );
      }
    } finally {
      autoApplyInFlight.current = false;
      setBusy(false);
    }
  };
  useEffect(() => {
    let disposed = false;
    const removers: Array<() => void> = [];
    void (async () => {
      const installed = await Promise.allSettled([
        listen<ipc.AutoScanStatusDto>('autoscan-status', ({ payload }) => {
          if (disposed) return;
          const accepted = acceptAutoScanStatus(payload, 'event');
          if (accepted) setStatus(`AutoScan: ${payload.detail}`, payload.active ? '' : 'err');
        }),
        listen<ipc.AutoScanTriggerDto>('autoscan-trigger', ({ payload }) => {
          if (!disposed) void autoScanTriggerRef.current(payload);
        }),
      ]);
      for (const [index, result] of installed.entries()) {
        if (result.status === 'fulfilled') {
          if (disposed) result.value(); else removers.push(result.value);
        } else if (!disposed) {
          setStatus(
            `AutoScan ${index === 0 ? 'status' : 'trigger'} subscription failed: ${result.reason}`,
            'err',
          );
        }
      }
      if (disposed) return;
      try {
        acceptAutoScanStatus(await ipc.autoScanStatus(), 'snapshot');
      } catch (error) {
        if (!disposed) setStatus(`AutoScan status is unavailable: ${error}`, 'err');
      }
    })();
    return () => {
      disposed = true;
      for (const remove of removers) remove();
    };
  }, [acceptAutoScanStatus, setStatus]);

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
      if (confirmOpen) return;
      // F5 / F9 = the FFS compare / synchronize keys; Ctrl+R also compares
      if (e.key === 'F5') { e.preventDefault(); void doCompare(); }
      else if (e.key === 'F9') { e.preventDefault(); void openConfirm(); }
      else if (mod && e.key.toLowerCase() === 'r') { e.preventDefault(); void doCompare(); }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [ctx, editor, settingsOpen, askSwap, confirmOpen, doCompare, openConfirm]);

  // The funnel is anchored to its toolbar button and has no dismissal of its own; the context menu
  // brings its own (outside click, Esc, and any scroll under it), so it is not handled here
  useEffect(() => {
    const onClick = () => setFunnelAnchor(null);
    document.addEventListener('click', onClick);
    return () => document.removeEventListener('click', onClick);
  }, []);

  const hasRows = !!plan && plan.ops.length > 0;
  const reviewBusy = operationReviewPending(compareReview) || operationReviewPending(applyReview);
  const confirmTotals: ConfirmTotals | null = useMemo(() => {
    if (!plan) return null;
    const t: ConfirmTotals = { copy: 0, update: 0, move: 0, del: 0, bytes: 0, delBytes: 0, flips: 0, hiddenChecked: 0 };
    for (const i of final) {
      const op = eff(plan, flipped, i);
      if (op.action === 'copy') { t.copy++; t.bytes += op.size ?? 0; }
      else if (op.action === 'update' || op.action === 'chmod') { t.update++; t.bytes += op.size ?? 0; }
      else if (op.action === 'move') t.move++;
      else if (op.action === 'delete' || op.action === 'delete_dir') { t.del++; t.delBytes += op.size ?? 0; }
      if (flipped[i]) t.flips++;
    }
    t.hiddenChecked = checked.filter(Boolean).length - final.length;
    return t;
  }, [plan, final, flipped, checked]);

  return (
    <>
      <div className="app">
        <Sidebar
          jobs={jobs}
          currentJobId={currentJob?.job_id ?? null}
          lastMap={lastMap}
          busy={busy}
          reviewing={reviewBusy}
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
            busy={busy || reviewBusy}
            canSync={final.length > 0}
            watchStatus={autoScanStatus}
            watchPending={autoScanControlPending}
            onCompare={() => void doCompare()}
            onSync={() => void openConfirm()}
            onToggleLog={() => setLogOpen((v) => !v)}
            onToggleWatch={() => {
              const action = autoScanToggleAction(autoScanStatusRef.current, currentJob !== null);
              if (action === 'stop') { stopAutoScan(); return; }
              if (action !== 'start' || !currentJob || autoScanControlPendingRef.current !== null) return;
              const monitoredJob = currentJob;
              const monitoredTarget = selTarget;
              const request = autoScanControlRequest.current + 1;
              autoScanControlRequest.current = request;
              autoScanTicket.current = null;
              autoScanControlPendingRef.current = 'start';
              setAutoScanControlPending('start');
              setStatus(`Starting AutoScan for '${monitoredJob.name}'…`);
              void ipc.startAutoScan(
                monitoredJob.job_id,
                monitoredJob.config_revision,
                monitoredTarget,
              ).then((next) => {
                if (autoScanControlRequest.current !== request) return;
                if (acceptAutoScanStatus(next, 'start')) {
                  setStatus(`AutoScan: ${next.detail}${next.auto_apply ? ' · unattended apply requires an exact prior grant' : ''}`);
                }
              }).catch((error) => {
                if (autoScanControlRequest.current !== request) return;
                setStatus(`AutoScan could not start: ${error}`, 'err');
              }).finally(() => {
                if (autoScanControlRequest.current === request) {
                  autoScanControlPendingRef.current = null;
                  setAutoScanControlPending(null);
                }
              });
            }}
          />
          <PathLine
            job={currentJob}
            cfgJob={cfgJob}
            busy={busy}
            reviewing={reviewBusy}
            selTarget={selTarget}
            pathHistory={pathHistory}
            dropOn={dropOn === 'source' || dropOn === 'target' ? dropOn : null}
            scopeRef={setPathScope}
            onCommit={(which, v) => void saveRoot(which, v)}
            onBrowse={(which) => void browseRoot(which)}
            onSwap={() => void requestSwap()}
            onSelectTarget={(i) => {
              if (busy || i === selTarget) return;
              const selected = currentJob;
              if (!selected) return;
              selectionRef.current = { job: selected, targetIndex: i };
              setSelTarget(i);
              resetNavigationUi();
              const t = selected.targets[i] ?? '';
              const restored = sessionForSelection(compareRepository, selected, i);
              setStatus(restored
                ? `Switched target → ${t} · restored ${restored.plan.ops.length} compare items`
                : `Switched target → ${t} — Compare again (Ctrl+R)`);
              requestResultRestore(selected, i, restored);
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
              searchKey={currentJob?.job_id ?? ''}
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
                    if (!cmpRunReady.current || cmpRunId.current < 0) {
                      setStatus('Compare is still starting — cancel will be available when its run is registered');
                      return;
                    }
                    const runId = cmpRunId.current;
                    setCmpCancelling(true);
                    setStatus('Cancelling the compare…');
                    ipc.cancelRun(runId).then((accepted) => {
                      if (accepted) return;
                      setCmpCancelling(false);
                      setStatus('That compare already finished; no newer run was cancelled');
                    }).catch((e) => {
                      setCmpCancelling(false);
                      setStatus(`Cancel failed: ${e}`, 'err');
                    });
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
                  resetKey={`${currentJob?.job_id}|${search}|${[...chips].join()}|${ovFilter}|${sort?.key}${sort?.dir}|${grouped}`}
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
          onClear={() => { setVfilter(EMPTY_FILTER); clearMasks(); }}
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
          onSaved={async (saved, job, original) => {
            const selectedIdentity = !!original && currentJob?.job_id === original.jobId;
            const semanticMutation = !!original
              && saved.config_revision !== original.configRevision;
            const preserveRuntime = !!original
              && !semanticMutation
              && (saved.effect === 'renamed' || saved.effect === 'updated' || saved.effect === 'no_op');
            setEditor(null);
            if (original) {
              setCompareRepository((repository) => reconcileSavedJobSession(
                repository,
                saved,
                original,
              ));
            }
            if (original && !preserveRuntime) {
              resetSafetyUi();
            }
            pushHistory(job.source);
            pushHistory(job.target);
            try {
              await refreshJobs(selectedIdentity ? original?.name : undefined);
              if (selectedIdentity) setCfgJob(job);
              setStatus(
                saved.effect === 'no_op' ? `No changes to save for '${saved.name}'` : `Saved '${saved.name}'`,
                'ok',
              );
            } catch (e) {
              setStatus(`Saved '${saved.name}', but refreshing the job list failed: ${e}`, 'err');
            }
          }}
          onDeleted={async (deleted) => {
            setEditor(null);
            invalidateCompareJob(deleted.job_id);
            resetSafetyUi();
            if (currentJob?.job_id === deleted.job_id) {
              setCurrentJob(null);
              setSelTarget(0);
            }
            try {
              await refreshJobs();
              setStatus(`Deleted '${deleted.name}'`);
            } catch (e) {
              setStatus(`Deleted '${deleted.name}', but refreshing the job list failed: ${e}`, 'err');
            }
          }}
          onMutationConflict={async (name, original) => {
            const list = await refreshJobs(name);
            if (!original) return;
            const refreshed = list.find((candidate) => candidate.job_id === original.jobId) ?? null;
            setCompareRepository((repository) => reconcileRefreshedJobSession(repository, original, refreshed));
            if (currentJob?.job_id === original.jobId
              && (!refreshed || currentJob.config_revision !== refreshed.config_revision)) {
              resetNavigationUi();
            }
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
            `source ← ${(askSwap.job.targets.length ? askSwap.job.targets : [askSwap.job.target])[askSwap.targetIndex]}\n` +
            `target ${askSwap.targetIndex + 1} ← ${askSwap.job.source}\n\n` +
            (askSwap.job.mode === 'mirror'
              ? 'In mirror mode this reverses which side is authoritative: after the swap, the original target wins.\n\n'
              : '') +
            'The job file is rewritten and the current compare result is discarded. The status bar keeps an undo.'
          }
          actions={[{
            label: 'Swap them',
            onConfirm: () => void doSwap(
              askSwap.jobId,
              askSwap.name,
              askSwap.job,
              askSwap.configRevision,
              askSwap.targetIndex,
            ),
          }]}
          onCancel={() => setAskSwap(null)}
        />
      )}
      {confirmOpen && currentJob && confirmTotals && (
        <ConfirmSheet
          job={currentJob}
          totals={confirmTotals}
          reviewState={applyReview}
          choices={applyChoices}
          onChoices={setApplyChoices}
          onCancel={resetConfirmation}
          onConfirm={() => void doSync()}
        />
      )}
      {compareReview.review && compareReview.review.status !== 'direct_authorized' && (
        <CompareReviewSheet
          state={compareReview}
          choices={compareChoices}
          onChoices={setCompareChoices}
          onCancel={resetCompareReview}
          onApprove={() => void approveCompareReview()}
        />
      )}
    </>
  );
}
