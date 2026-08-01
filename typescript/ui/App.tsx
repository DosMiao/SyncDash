// This orchestration shell owns session selection, compare/apply reviews, and mutating Tauri
// workflows. Result semantics live in core, effectful domain state in hooks/state, and rendering in
// components. Execution receives only a one-use authorization token; Rust owns the authenticated
// plan and reconstructs every operation.

import { useCallback, useEffect, useId, useMemo, useReducer, useRef, useState } from 'react';
import { CircleCheck, FolderSearch } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';
import { getCurrentWebview } from '@tauri-apps/api/webview';

import * as ipc from '../core/ipc';
import { owningFolderOf } from '../core/folders';
import { buildLayout, flattenLayout, layoutFolderPaths } from '../core/grouping';
import { addExcludeEntries } from '../core/junk';
import { baseOf, fullPath, p2 } from '../core/format';
import {
  canReverseOperation,
  effectiveOperation,
  keySpec,
  rowMetadata,
  rowTransferBytes,
  isExecutableOperation,
  selectedRows,
  sidePaths,
} from '../core/plan';
import {
  computeExecutableIndices,
  computeInScopeIndices,
  countActiveAdvancedFilterGroups,
  matchesFolderScope,
} from '../core/runScope';
import { reduceCompareStages } from '../core/compareProgress';
import type { PlanDto, Sort, SortKey } from '../core/plan';
import type { CmpStage, CompareProgressEvent } from '../core/compareProgress';
import type { PlanLayout } from '../core/grouping';
import type { JobDto } from '../core/types/generated/JobDto';
import type { RunRecord } from '../core/types/generated/RunRecord';
import type { AutoScanStatusDto } from '../core/types/generated/AutoScanStatusDto';
import type { AutoScanTriggerDto } from '../core/types/generated/AutoScanTriggerDto';

import { useStatus } from './hooks/useStatus';
import { useRunScopeController } from './hooks/useRunScopeController';
import { useZoomControl } from './hooks/useZoomControl';
import {
  activeSession as sessionForSelection,
  compareScopeKey,
  EMPTY_COMPARE_REPOSITORY,
  invalidateJobRevision,
  invalidateSession,
  invalidateJobSession,
  ownerMatchesSelection,
  retainConfirmedSession,
  reconcileRefreshedJobSession,
  reconcileSavedJobSession,
  retainRestoredSession,
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
  operationApprovalFromChoices,
  operationReviewCanSubmit,
  operationReviewPending,
  operationReviewReducer,
  ownsOperationReviewRequest,
  type ApprovalChoices,
  type ReviewRequestFence,
} from './state/operationReview';
import { RequestFence } from './state/request-fence';
import { mergeRunEventReplay } from '../core/runEvents';
import { ComparePanel } from './components/ComparePanel';
import { ScanFaultBanner } from './components/ScanFaultBanner';
import { ConfirmSheet } from './components/ConfirmSheet';
import { CompareReviewSheet } from './components/OperationReviewSheet';
import { ResultBar } from './components/ResultBar';
import { AdvancedFiltersPopover } from './components/AdvancedFiltersPopover';
import { JobEditor } from './components/JobEditor';
import { LogPanel } from './components/LogPanel';
import { RunScopePanel } from './components/RunScopePanel';
import { PathLine } from './components/PathLine';
import { PlanTable } from './components/PlanTable';
import { IdenticalResultsPanel } from './components/IdenticalResultsPanel';
import { SettingsSheet } from './components/SettingsSheet';
import { Sidebar } from './components/Sidebar';
import { StatusBar } from './components/StatusBar';
import { Toolbar } from './components/Toolbar';
import { ConfirmDialog, ContextMenu, MenuDivider, MenuItem, Placeholder } from './components/ui';
import type { ApplyReviewTotals } from './components/ConfirmSheet';
import type { EditorApi } from './components/JobEditor';
import { deriveApplyAvailability } from './state/result-workspace';
import type { ResultView } from './state/result-workspace';

const HIST_KEY = 'sd.pathhist';

/// Stable identity for "no plan, nothing to lay out" — a fresh object literal here would make the
/// flatten memo below recompute on every render
const EMPTY_LAYOUT: PlanLayout = { displayOrder: [], folderTree: null };
const EMPTY_FLAGS: boolean[] = [];

/// One entry in a row's right-click menu. Built at open time so each closure sees the row and the
/// plan as they were when you right-clicked — a menu is transient, and a stale entry would be worse
/// than a frozen one.
interface ContextMenuEntry {
  label: string;
  disabled?: boolean;
  danger?: boolean;
  separator?: boolean;
  run?: () => void;
}

interface ContextMenuState { x: number; y: number; entries: ContextMenuEntry[] }
interface CompareCompletion { plan: PlanDto }
interface AutoScanOutcome { completion: CompareCompletion | null; owned: boolean }

function readHistory(): string[] {
  try { return JSON.parse(localStorage.getItem(HIST_KEY) ?? '[]') as string[]; } catch { return []; }
}

export function App() {
  const [jobs, setJobs] = useState<JobDto[]>([]);
  const [currentJob, setCurrentJob] = useState<JobDto | null>(null);
  const [jobConfiguration, setJobConfiguration] = useState<ipc.JobFull | null>(null);
  const [lastSyncByJobName, setLastSyncByJobName] = useState<Record<string, RunRecord>>({});
  const [appVersion, setAppVersion] = useState('');
  const [jobsDir, setJobsDir] = useState('');
  const [pathHistory, setPathHistory] = useState<string[]>(readHistory);
  const [selectedTargetIndex, setSelectedTargetIndex] = useState(0);

  const [compareRepository, setCompareRepository] = useState<CompareRepository>(EMPTY_COMPARE_REPOSITORY);
  const restoreRequest = useRef(new RequestFence());
  const [busy, setBusy] = useState(false);
  const applyExecutionRequest = useRef<ReviewRequestFence | null>(null);
  const autoApplyInFlight = useRef(false);

  const [sort, setSort] = useState<Sort | null>(null);
  const [pathMode, setPathMode] = useState<'rel' | 'full'>(() => (localStorage.getItem('sd.pathmode') === 'full' ? 'full' : 'rel'));
  const [grouped, setGrouped] = useState(() => localStorage.getItem('sd.grouped') !== 'off');
  const [collapsedFolderPaths, setCollapsedFolderPaths] = useState<Set<string>>(new Set());

  const [editor, setEditor] = useState<{ name: string | null; focusGroup?: string } | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [confirmReviewKey, setConfirmReviewKey] = useState<string | null>(null);
  const [applyReview, dispatchApplyReview] = useReducer(operationReviewReducer, INITIAL_OPERATION_REVIEW);
  const [applyChoices, setApplyChoices] = useState<ApprovalChoices>(EMPTY_APPROVAL_CHOICES);
  const applyReviewRequestId = useRef(0);
  const applyReviewRequest = useRef<ReviewRequestFence | null>(null);
  const confirmReviewKeyRef = useRef<string | null>(null);
  const [compareReview, dispatchCompareReview] = useReducer(operationReviewReducer, INITIAL_OPERATION_REVIEW);
  const [compareChoices, setCompareChoices] = useState<ApprovalChoices>(EMPTY_APPROVAL_CHOICES);
  const compareReviewRequestId = useRef(0);
  const compareReviewRequest = useRef<ReviewRequestFence | null>(null);
  const compareReviewFetchRequest = useRef<ReviewRequestFence | null>(null);
  const compareApprovalRequest = useRef<ReviewRequestFence | null>(null);
  const [advancedFiltersAnchor, setAdvancedFiltersAnchor] = useState<DOMRect | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [logOpen, setLogOpen] = useState(false);
  const [logReload, setLogReload] = useState(0);
  const [resultView, setResultView] = useState<ResultView>('differences');
  const [dropTargetKey, setDropTargetKey] = useState<string | null>(null);
  const [resultsViewport, setResultsViewport] = useState<HTMLDivElement | null>(null);
  // Retain the exact job snapshot so root-swap confirmation names what it will mutate.
  const [askSwap, setAskSwap] = useState<{
    jobId: string;
    name: string;
    job: ipc.JobFull;
    configRevision: string;
    targetIndex: number;
  } | null>(null);
  // The drag handler is registered once and must read the live droppable regions at drop time.
  const dropScope = useRef<{ editor: HTMLElement | null; path: HTMLElement | null }>({ editor: null, path: null });
  // Stable identities: a ref callback whose identity changes is detached with null and reattached
  // on every render, and these two are handed to components that re-render on every keystroke.
  const setPathScope = useCallback((element: HTMLElement | null) => { dropScope.current.path = element; }, []);
  const setEditorScope = useCallback((element: HTMLElement | null) => { dropScope.current.editor = element; }, []);

  const [compareActive, setCompareActive] = useState(false);
  const [compareStages, setCompareStages] = useState<CmpStage[]>([]);
  const [compareCancelling, setCompareCancelling] = useState(false);
  // The 0.7/0.3 EMA prevents per-file size swings from dominating the compare rate.
  const compareRateByPhase = useRef(new Map<string, {
    timestampMs: number;
    bytesDone: number;
    smoothedRate: number;
  }>());
  const compareRunId = useRef(-1);
  const compareRunFloor = useRef(-1);
  const compareRunReady = useRef(false);
  const compareInFlight = useRef(false);

  // AutoScan is backend-owned. The webview renders status and handles exact trigger tickets; it
  // never owns the clock or assumes that remaining mounted means the watcher is still alive.
  const [autoScanStatus, setAutoScanStatus] = useState<AutoScanStatusDto | null>(null);
  const autoScanStatusRef = useRef<AutoScanStatusDto | null>(null);
  const autoScanTicket = useRef<AutoScanTicket | null>(null);
  const autoScanLedger = useRef(new AutoScanTicketLedger<AutoScanOutcome>());
  const autoScanControlRequest = useRef(0);
  const [autoScanControlPending, setAutoScanControlPending] = useState<'start' | 'stop' | null>(null);
  const autoScanControlPendingRef = useRef<'start' | 'stop' | null>(null);
  const autoScanTriggerRef = useRef<(trigger: AutoScanTriggerDto) => Promise<void>>(async () => {});

  const editorApi = useRef<EditorApi | null>(null);
  const { status, set: setStatus, withUndo: setStatusUndo, runUndo } = useStatus('');
  const zoom = useZoomControl();
  const selectionRef = useRef<{ job: JobDto | null; targetIndex: number }>({ job: null, targetIndex: 0 });
  selectionRef.current = { job: currentJob, targetIndex: selectedTargetIndex };

  const activeCompare = sessionForSelection(compareRepository, currentJob, selectedTargetIndex);
  const plan = activeCompare?.plan ?? null;
  const checked = activeCompare?.checked ?? EMPTY_FLAGS;
  const flipped = activeCompare?.flipped ?? EMPTY_FLAGS;
  const reportRunScopeError = useCallback((message: string) => setStatus(message, 'err'), [setStatus]);
  const runScope = useRunScopeController(plan, flipped, reportRunScopeError);
  const {
    selectedResultTypes,
    setSelectedResultTypes,
    searchDraft,
    setSearchDraft,
    searchQuery,
    searchPending,
    clearSearch,
    folderScope,
    setFolderScope,
    advancedFilter,
    setAdvancedFilter,
    maskDraft,
    setMaskDraft,
    clearAdvancedFilter: clearAdvancedScopeCriteria,
    excludedByMask,
    scopeCalculationPending,
    scopeCalculationFailed,
    clearRunScope: clearRunScopeCriteria,
    resetResultWorkspace: resetRunScopeWorkspace,
    expandedFolders,
    toggleExpandedFolder,
    panelCollapsed,
    togglePanelCollapsed,
  } = runScope;
  const resultPanelId = useId();
  const differencesTabId = `${resultPanelId}-differences-tab`;
  const identicalTabId = `${resultPanelId}-identical-tab`;

  // Three memos, not one, because the three questions change at different rates. Membership is the
  // expensive full-table scan and no longer depends on `sort`, so clicking a header does not re-run
  // it; the layout does the sorting; flattening only decides which member rows a fold emits, so
  // folding one directory costs one pass instead of redoing the sort.
  //
  // `flipped` legitimately appears in all three: reversal changes the effective operation, its
  // owning folder, side paths, and sort key. It therefore requires a complete derived-state rebuild.
  const inScopeIndices = useMemo(() => (
    plan ? computeInScopeIndices({
      plan,
      flipped,
      selectedResultTypes,
      searchQuery,
      folderScope,
      advancedFilter,
      excludedByMask,
    }) : []
  ), [plan, flipped, selectedResultTypes, searchQuery, folderScope, advancedFilter, excludedByMask]);

  const executableIndices = useMemo(() => (
    plan ? computeExecutableIndices(plan, flipped, inScopeIndices, checked) : []
  ), [plan, flipped, inScopeIndices, checked]);
  const reviewedRows = useMemo(
    () => (plan ? selectedRows(executableIndices, flipped) : []),
    [plan, executableIndices, flipped],
  );
  const reviewKey = useMemo(() => (
    plan && currentJob
      ? applyReviewKey(plan.owner.identity, currentJob.job_id, currentJob.config_revision, selectedTargetIndex, reviewedRows)
      : null
  ), [plan, currentJob, selectedTargetIndex, reviewedRows]);
  const currentReviewKeyRef = useRef<string | null>(reviewKey);
  currentReviewKeyRef.current = reviewKey;
  const currentCompareReviewKey = useMemo(() => (
    currentJob
      ? compareReviewKey(currentJob.job_id, currentJob.config_revision, selectedTargetIndex)
      : null
  ), [currentJob, selectedTargetIndex]);
  const currentCompareReviewKeyRef = useRef<string | null>(currentCompareReviewKey);
  currentCompareReviewKeyRef.current = currentCompareReviewKey;

  const layout = useMemo(() => (
    plan ? buildLayout({ plan, flipped, inScopeIndices, grouped, sort }) : EMPTY_LAYOUT
  ), [plan, flipped, inScopeIndices, grouped, sort]);

  const rowPlan = useMemo(() => flattenLayout(layout, collapsedFolderPaths), [layout, collapsedFolderPaths]);
  const folderPathsInLayout = useMemo(() => layoutFolderPaths(layout), [layout]);
  // A filter may temporarily remove a collapsed branch. Only keys present in this layout decide
  // whether the toolbar says Expand all; otherwise one stale path leaves the control backwards.
  const anyCollapsed = useMemo(
    () => folderPathsInLayout.some((folderPath) => collapsedFolderPaths.has(folderPath)),
    [folderPathsInLayout, collapsedFolderPaths],
  );

  // Fold state belongs to one compare result. A new plan can reuse the same relative names for
  // entirely different roots, so carrying old folds over would hide fresh results on arrival.
  useEffect(() => { setCollapsedFolderPaths(new Set()); }, [plan]);

  const stats = useMemo(() => {
    if (!plan) return null;
    const next = {
      copyCount: 0,
      updateCount: 0,
      moveCount: 0,
      deleteCount: 0,
      transferBytes: 0,
      reversedCount: 0,
    };
    for (const index of executableIndices) {
      const operation = effectiveOperation(plan, flipped, index);
      switch (operation.action) {
        case 'copy': next.copyCount++; next.transferBytes += rowTransferBytes(plan, flipped, index); break;
        case 'update': next.updateCount++; next.transferBytes += rowTransferBytes(plan, flipped, index); break;
        case 'chmod': next.updateCount++; break;
        case 'move': next.moveCount++; break;
        case 'delete': case 'delete_dir': next.deleteCount++; break;
      }
      if (flipped[index]) next.reversedCount++;
    }
    return next;
  }, [plan, executableIndices, flipped]);

  const applyAvailability = useMemo(() => deriveApplyAvailability({
    hasPlan: plan !== null,
    resultView,
    scopeCalculationPending,
    scopeCalculationFailed,
    executableCount: executableIndices.length,
  }), [plan, resultView, scopeCalculationPending, scopeCalculationFailed, executableIndices.length]);

  const pushHistory = useCallback((candidatePath: string) => {
    const normalizedPath = candidatePath.trim();
    if (!normalizedPath) return;
    setPathHistory((previousPaths) => {
      const nextPaths = [
        normalizedPath,
        ...previousPaths.filter((path) => path.toLowerCase() !== normalizedPath.toLowerCase()),
      ].slice(0, 12);
      localStorage.setItem(HIST_KEY, JSON.stringify(nextPaths));
      return nextPaths;
    });
  }, []);

  const refreshJobs = useCallback(async (keepName?: string) => {
    const list = await ipc.listJobs();
    setJobs(list);
    if (keepName) {
      // listJobs is the authoritative registry. The name guard prevents a delayed refresh from
      // hijacking a newer selection; when that guarded job disappeared, retaining `cur` would
      // instead leave a ghost row that every later Compare retries.
      setCurrentJob((selectedJob) => (
        selectedJob?.name === keepName
          ? list.find((job) => job.job_id === selectedJob.job_id) ?? null
          : selectedJob
      ));
    }
    return list;
  }, []);

  const refreshLastSyncs = useCallback(() => {
    ipc.lastSyncs().then(setLastSyncByJobName).catch(() => { /* missing logs are not fatal */ });
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
    applyReviewRequest.current = null;
    confirmReviewKeyRef.current = null;
    setApplyChoices(EMPTY_APPROVAL_CHOICES);
    setConfirmOpen(false);
    setConfirmReviewKey(null);
    dispatchApplyReview({ type: 'reset' });
  }, []);

  const resetCompareReview = useCallback(() => {
    compareReviewRequest.current = null;
    compareReviewFetchRequest.current = null;
    compareApprovalRequest.current = null;
    setCompareChoices(EMPTY_APPROVAL_CHOICES);
    dispatchCompareReview({ type: 'reset' });
  }, []);

  const resetSafetyUi = useCallback(() => {
    resetConfirmation();
    resetCompareReview();
    setResultView('differences');
    setAdvancedFiltersAnchor(null);
    setContextMenu(null);
    setAskSwap(null);
  }, [resetCompareReview, resetConfirmation]);

  const clearAdvancedFilters = useCallback(() => {
    clearAdvancedScopeCriteria();
    setAdvancedFiltersAnchor(null);
  }, [clearAdvancedScopeCriteria]);

  const clearRunScope = useCallback(() => {
    clearRunScopeCriteria();
    setAdvancedFiltersAnchor(null);
  }, [clearRunScopeCriteria]);

  const acceptAutoScanStatus = useCallback((
    incoming: AutoScanStatusDto,
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

  const resetResultWorkspace = useCallback(() => {
    resetSafetyUi();
    resetRunScopeWorkspace();
    setSort(null);
    setCollapsedFolderPaths(new Set());
  }, [resetRunScopeWorkspace, resetSafetyUi]);

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
    if (!currentJob || currentJob.targets.length === 0 || selectedTargetIndex < currentJob.targets.length) return;
    setSelectedTargetIndex(0);
    resetResultWorkspace();
    setStatus(`'${currentJob.name}' no longer has target ${selectedTargetIndex + 1}; selected target 1`);
  }, [currentJob, selectedTargetIndex, resetResultWorkspace, setStatus]);

  const invalidateCompareRevision = useCallback((jobId: string, configRevision: string) => {
    setCompareRepository((repository) => invalidateJobRevision(repository, jobId, configRevision));
  }, []);

  const invalidateCompareJob = useCallback((jobId: string) => {
    setCompareRepository((repository) => invalidateJobSession(repository, jobId));
  }, []);

  const setChecked = useCallback((next: boolean[] | ((prev: boolean[]) => boolean[])) => {
    setCompareRepository((repository) => updateSession(repository, currentJob, selectedTargetIndex, (session) => ({
      ...session,
      checked: typeof next === 'function' ? next(session.checked) : next,
    })));
  }, [currentJob, selectedTargetIndex]);

  const setFlipped = useCallback((next: boolean[] | ((prev: boolean[]) => boolean[])) => {
    setCompareRepository((repository) => updateSession(repository, currentJob, selectedTargetIndex, (session) => ({
      ...session,
      flipped: typeof next === 'function' ? next(session.flipped) : next,
    })));
  }, [currentJob, selectedTargetIndex]);

  const requestResultRestore = useCallback((
    job: JobDto,
    targetIndex: number,
    retained: ReturnType<typeof sessionForSelection>,
    announce = true,
  ) => {
    const ticket = restoreRequest.current.start(compareScopeKey(job.job_id, targetIndex, job.config_revision));
    const publish = (restored: PlanDto | null) => {
      if (!restoreRequest.current.owns(ticket) || !restored) return;
      const selected = selectionRef.current;
      if (!ownerMatchesSelection(restored.owner, selected.job, selected.targetIndex)) return;
      const session = successfulSession(
        restored,
        restored.ops.map((operation) => isExecutableOperation(operation)),
        restored.ops.map(() => false),
      );
      setCompareRepository((repository) => retainRestoredSession(repository, session));
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
    void ipc.touchCompare(retained.plan.owner).then((backendOwner) => {
      if (!restoreRequest.current.owns(ticket)) return;
      if (!backendOwner) {
        setCompareRepository((repository) => invalidateSession(repository, retained.plan.owner));
        if (announce) setStatus(`${job.name} · retained result expired — Compare again`, 'err');
        return;
      }
      setCompareRepository((repository) => retainConfirmedSession(
        repository,
        retained,
        backendOwner,
      ));
    }).catch(failed);
  }, [setStatus]);

  const runAuthorizedCompare = useCallback(async (
    authorizationToken: string,
    comparedJob: JobIdentitySnapshot,
    targetIndex: number,
    autoTicket?: AutoScanTicket,
  ): Promise<CompareCompletion | null> => {
    if (busy || editor || compareInFlight.current || autoApplyInFlight.current) return null;
    if (autoTicket && (
      !monitorOwnsAutoScanTicket(autoScanStatusRef.current, autoScanTicket.current, autoTicket)
      || applyExecutionRequest.current !== null
      || applyReviewRequest.current !== null
      || compareReviewRequest.current !== null
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
    if (showProgress) setCompareStages([]);
    compareRateByPhase.current.clear();
    if (showProgress) setCompareCancelling(false);
    compareRunFloor.current = compareRunId.current;
    compareRunReady.current = false;
    if (showProgress) setCompareActive(true);
    try {
      const comparedPlan = await ipc.compareJob(authorizationToken);
      restoreRequest.current.invalidateOwner(compareScopeKey(
        comparedPlan.owner.identity.job_id,
        comparedPlan.owner.identity.target_index,
        comparedPlan.owner.identity.config_revision,
      ));
      const freshFlips = comparedPlan.ops.map(() => false);
      setCompareRepository((repository) => retainSuccessfulSession(
        repository,
        successfulSession(
          comparedPlan,
          comparedPlan.ops.map((operation) => isExecutableOperation(operation)),
          freshFlips,
        ),
      ));
      // A job file can be edited outside the app while it is open. Compare used the authoritative
      // file, so refresh the list row before deciding whether the returned owner belongs on screen.
      // Snapshot first: refreshJobs may commit the new row (or null) before this continuation runs,
      // and then the ref no longer tells us that the selected job changed underneath this compare.
      const selectedBeforeRefresh = selectionRef.current;
      let refreshedJob: JobDto | null = null;
      let refreshProblem: unknown = null;
      try {
        const list = await refreshJobs(name);
        refreshedJob = list.find((job) => job.job_id === comparedPlan.owner.identity.job_id) ?? null;
        setCompareRepository((repository) => reconcileRefreshedJobSession(
          repository,
          {
            jobId: comparedPlan.owner.identity.job_id,
            name: comparedPlan.owner.job_name,
            configRevision: comparedPlan.owner.identity.config_revision,
          },
          refreshedJob,
        ));
      } catch (error) {
        refreshProblem = error;
      }
      const selected = selectionRef.current;
      const selectedJob = selected.job?.job_id === comparedPlan.owner.identity.job_id && !refreshProblem
        ? refreshedJob
        : selected.job;
      if (selectedBeforeRefresh.job?.job_id === comparedPlan.owner.identity.job_id && !refreshProblem) {
        if (!refreshedJob) {
          setSelectedTargetIndex(0);
          resetResultWorkspace();
        } else if (selectedBeforeRefresh.job.config_revision !== refreshedJob.config_revision) {
          resetResultWorkspace();
        }
      }
      const resultBelongsToSelection = ownerMatchesSelection(
        comparedPlan.owner,
        selectedJob,
        selected.targetIndex,
      );
      if (resultBelongsToSelection) {
        setStatus(
          comparedPlan.ops.length === 0
            ? 'No differences in the compared scope'
            : `${comparedPlan.ops.length} items · ${comparedPlan.header.conflict_count} conflicts`,
          comparedPlan.header.conflict_count > 0 ? 'err' : '',
        );
      } else if (refreshProblem) {
        setStatus(`Compare finished for '${name}', but the refreshed job identity could not be read: ${refreshProblem}`, 'err');
      } else if (autoTicket) {
        setStatus(
          comparedPlan.ops.length === 0
            ? `AutoScan finished for '${name}' — no differences in the compared scope; result retained`
            : `AutoScan finished for '${name}' — ${comparedPlan.ops.length} differences retained for review`,
          comparedPlan.header.conflict_count > 0 ? 'err' : '',
        );
      } else {
        setStatus(`Compare finished for '${name}', but its job or target changed — the result was not attached to the current view`, 'err');
      }
      return { plan: comparedPlan };
    } catch (error) {
      const cancelled = String(error) === 'cancelled';
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
            setSelectedTargetIndex(0);
            resetResultWorkspace();
            suffix = ` · '${name}' is no longer a registered job`;
          } else if (selected.job.config_revision !== refreshedJob.config_revision) {
            resetResultWorkspace();
            suffix = ' · refreshed the changed job configuration';
          }
        }
      } catch (refreshError) {
        refreshProblem = refreshError;
      }
      const base = cancelled ? 'Compare cancelled' : `${autoTicket ? 'AutoScan Compare' : 'Compare'} failed: ${error}`;
      if (refreshProblem) suffix = ` · job-list refresh failed: ${refreshProblem}`;
      setStatus(`${base}${suffix}`, cancelled && !refreshProblem ? '' : 'err');
      return null;
    } finally {
      if (showProgress) setCompareActive(false);
      setBusy(false);
      compareRunReady.current = false;
      compareInFlight.current = false;
    }
  }, [busy, editor, confirmOpen, compareReview, applyReview, refreshJobs, resetResultWorkspace, resetSafetyUi, setStatus]);

  const doCompare = useCallback(async (autoTicket?: AutoScanTicket): Promise<CompareCompletion | null> => {
    if (busy || editor || compareInFlight.current || applyExecutionRequest.current || autoApplyInFlight.current) return null;
    if (!autoTicket && !currentJob) return null;
    if (!autoTicket && (applyReviewRequest.current || compareReviewRequest.current)) return null;
    if (!autoTicket && operationReviewPending(compareReview)) return null;
    if (autoTicket && (
      !monitorOwnsAutoScanTicket(autoScanStatusRef.current, autoScanTicket.current, autoTicket)
      || applyReviewRequest.current !== null
      || compareReviewRequest.current !== null
      || confirmOpen
      || operationReviewPending(compareReview)
      || operationReviewPending(applyReview)
    )) return null;

    const comparedJob: JobIdentitySnapshot = autoTicket
      ? { jobId: autoTicket.jobId, name: autoTicket.jobName, configRevision: autoTicket.configRevision }
      : snapshotJob(currentJob!);
    const targetIndex = autoTicket?.targetIndex ?? selectedTargetIndex;
    const key = compareReviewKey(comparedJob.jobId, comparedJob.configRevision, targetIndex);

    if (autoTicket) {
      setStatus(`AutoScan is reviewing Compare authorization for '${comparedJob.name}'…`);
      try {
        const review = await ipc.reviewCompare(comparedJob.jobId, targetIndex, {
          generation: autoTicket.generation,
          ticket_id: autoTicket.ticketId,
        });
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

    if (compareReviewFetchRequest.current?.key === key) return null;

    const request: ReviewRequestFence = {
      key,
      requestId: compareReviewRequestId.current + 1,
    };
    compareReviewRequestId.current = request.requestId;
    compareReviewRequest.current = request;
    compareReviewFetchRequest.current = request;
    setCompareChoices(EMPTY_APPROVAL_CHOICES);
    dispatchCompareReview({ type: 'begin', request });
    setStatus(`Reviewing Compare authorization for '${comparedJob.name}'…`);
    try {
      const review = await ipc.reviewCompare(comparedJob.jobId, targetIndex);
      if (!ownsOperationReviewRequest(compareReviewRequest.current, request, currentCompareReviewKeyRef.current)) {
        return null;
      }
      if (review.status === 'interactive_apply_confirmation_required') {
        const error = 'The Compare review returned an Apply approval challenge and was rejected';
        dispatchCompareReview({ type: 'failed', request, error });
        setStatus(error, 'err');
        return null;
      }
      const authorization = directAuthorization(review);
      if (review.status === 'direct_authorized' && !authorization) {
        const error = 'The direct Compare review contradicted its capability report and was rejected';
        dispatchCompareReview({ type: 'failed', request, error });
        setStatus(error, 'err');
        return null;
      }
      dispatchCompareReview({ type: 'resolved', request, review });
      if (authorization) {
        return runAuthorizedCompare(
          authorization.authorization_token,
          comparedJob,
          targetIndex,
        );
      }
      if (review.status === 'blocked') {
        setStatus(`Compare is blocked for '${comparedJob.name}' — review the required fixes`, 'err');
      } else if (review.status === 'compare_confirmation_required') {
        setStatus(`Compare requires your approval for '${comparedJob.name}'`);
      }
      return null;
    } catch (error) {
      if (!ownsOperationReviewRequest(compareReviewRequest.current, request, currentCompareReviewKeyRef.current)) {
        return null;
      }
      dispatchCompareReview({ type: 'failed', request, error: String(error) });
      setStatus(`Compare authorization review failed: ${error}`, 'err');
      return null;
    } finally {
      if (compareReviewFetchRequest.current?.requestId === request.requestId
        && compareReviewFetchRequest.current.key === request.key) {
        compareReviewFetchRequest.current = null;
      }
    }
  }, [currentJob, busy, editor, compareReview, applyReview, confirmOpen, selectedTargetIndex, runAuthorizedCompare, setStatus]);

  const approveCompareReview = useCallback(async () => {
    const request = compareReviewRequest.current;
    const review = compareReview.review;
    if (!request || !review || !operationReviewCanSubmit(compareReview, compareChoices)) return;
    if (review.status !== 'compare_confirmation_required') return;
    if (compareApprovalRequest.current?.requestId === request.requestId
      && compareApprovalRequest.current.key === request.key) return;
    compareApprovalRequest.current = request;
    const choices = normalizeApprovalChoices(review, compareChoices);
    dispatchCompareReview({ type: 'begin_approval', request });
    setStatus('Authorizing this exact Compare operation…');
    try {
      const authorization = await ipc.approveOperation(
        review.challenge_id,
        operationApprovalFromChoices(review, choices),
      );
      if (!ownsOperationReviewRequest(compareReviewRequest.current, request, currentCompareReviewKeyRef.current)) return;
      const selected = selectionRef.current;
      if (!selected.job) return;
      dispatchCompareReview({ type: 'authorized', request, authorization });
      await runAuthorizedCompare(
        authorization.authorization_token,
        snapshotJob(selected.job),
        selected.targetIndex,
      );
    } catch (error) {
      if (!ownsOperationReviewRequest(compareReviewRequest.current, request, currentCompareReviewKeyRef.current)) return;
      dispatchCompareReview({ type: 'approval_failed', request, error: String(error) });
      setStatus(`Compare authorization failed: ${error}`, 'err');
    } finally {
      if (compareApprovalRequest.current?.requestId === request.requestId
        && compareApprovalRequest.current.key === request.key) {
        compareApprovalRequest.current = null;
      }
    }
  }, [compareChoices, compareReview, runAuthorizedCompare, setStatus]);

  const openConfirm = useCallback(async () => {
    if (!applyAvailability.available) {
      setStatus(applyAvailability.blockedMessage ?? 'Apply is unavailable', 'err');
      return;
    }
    if (!currentJob
      || !plan
      || !reviewKey
      || busy
      || autoApplyInFlight.current
      || operationReviewPending(applyReview)
      || applyReviewRequest.current
      || compareReviewFetchRequest.current
      || compareReview.review) return;
    const request: ReviewRequestFence = {
      key: reviewKey,
      requestId: applyReviewRequestId.current + 1,
    };
    applyReviewRequestId.current = request.requestId;
    applyReviewRequest.current = request;
    confirmReviewKeyRef.current = reviewKey;
    setConfirmReviewKey(reviewKey);
    setApplyChoices(EMPTY_APPROVAL_CHOICES);
    dispatchApplyReview({ type: 'begin', request });
    setConfirmOpen(true);
    try {
      const review = await ipc.reviewApply(plan.owner.identity, reviewedRows);
      if (!ownsOperationReviewRequest(applyReviewRequest.current, request, currentReviewKeyRef.current)) return;
      if (review.status === 'compare_confirmation_required') {
        dispatchApplyReview({
          type: 'failed',
          request,
          error: 'The Apply review returned a Compare approval challenge and was rejected',
        });
        return;
      }
      if (review.status === 'direct_authorized' && !directAuthorization(review)) {
        dispatchApplyReview({
          type: 'failed',
          request,
          error: 'The direct Apply review contradicted its capability report and was rejected',
        });
        return;
      }
      dispatchApplyReview({ type: 'resolved', request, review });
    } catch (error) {
      if (!ownsOperationReviewRequest(applyReviewRequest.current, request, currentReviewKeyRef.current)) return;
      dispatchApplyReview({ type: 'failed', request, error: String(error) });
    }
  }, [applyAvailability, currentJob, plan, reviewKey, busy, applyReview, compareReview.review, reviewedRows, setStatus]);

  const doSync = useCallback(async () => {
    if (
      !currentJob
      || !plan
      || !reviewKey
      || !confirmOpen
      || confirmReviewKey !== reviewKey
      || confirmReviewKeyRef.current !== reviewKey
      || !applyAvailability.available
      || !operationReviewCanSubmit(applyReview, applyChoices)
    ) {
      setStatus('Apply is unavailable until this exact reviewed action set is authorized', 'err');
      return;
    }
    const request = applyReviewRequest.current;
    const review = applyReview.review;
    if (!request || !review) return;
    if (busy || autoApplyInFlight.current || (applyExecutionRequest.current?.requestId === request.requestId
      && applyExecutionRequest.current.key === request.key)) return;
    applyExecutionRequest.current = request;
    const selected = reviewedRows;
    const applyingJob = currentJob;
    // Whether the progress window stays during a sync is its own Auto-close / When-finished business
    let launchId: number | null = null;
    let executionStarted = false;
    try {
      let authorization = directAuthorization(review);
      if (review.status === 'interactive_apply_confirmation_required') {
        const choices = normalizeApprovalChoices(review, applyChoices);
        dispatchApplyReview({ type: 'begin_approval', request });
        setStatus('Authorizing this exact apply operation…');
        try {
          authorization = await ipc.approveOperation(
            review.challenge_id,
            operationApprovalFromChoices(review, choices),
          );
        } catch (error) {
          if (ownsOperationReviewRequest(applyReviewRequest.current, request, currentReviewKeyRef.current)) {
            dispatchApplyReview({ type: 'approval_failed', request, error: String(error) });
            setStatus(`Apply authorization failed: ${error}`, 'err');
          }
          return;
        }
        if (!ownsOperationReviewRequest(applyReviewRequest.current, request, currentReviewKeyRef.current)) return;
        dispatchApplyReview({ type: 'authorized', request, authorization });
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
      const applyResult = await ipc.applyJob(authorization.authorization_token, launchId);
      setStatus(
        applyResult.cancelled
          ? `Stopped: cancelled after ${applyResult.done} run — re-checking...`
          : `Done: ${applyResult.done} run, ${applyResult.skipped} skipped, ${applyResult.errors} errors — re-checking...`,
        applyResult.errors ? 'err' : 'ok',
      );
      setBusy(false);
      refreshLastSyncs();
      setLogReload((k) => k + 1);
      if (applyExecutionRequest.current?.requestId === request.requestId
        && applyExecutionRequest.current.key === request.key) {
        applyExecutionRequest.current = null;
      }
      await doCompare();
    } catch (error) {
      setStatus(
        executionStarted
          ? `Apply failed and may have made partial changes: ${error} — Compare again before continuing`
          : `Apply did not start: ${error} — the reviewed result was retained`,
        'err',
      );
      setBusy(false);
      requestResultRestore(applyingJob, selectedTargetIndex, activeCompare, false);
    } finally {
      if (launchId !== null) void ipc.cancelProgressLaunch(launchId);
      if (applyExecutionRequest.current?.requestId === request.requestId
        && applyExecutionRequest.current.key === request.key) {
        applyExecutionRequest.current = null;
      }
    }
  }, [currentJob, activeCompare, plan, reviewKey, confirmOpen, confirmReviewKey, applyAvailability, applyReview, applyChoices, busy, reviewedRows, selectedTargetIndex, doCompare, refreshLastSyncs, invalidateCompareRevision, requestResultRestore, resetConfirmation, resetSafetyUi, setStatus]);

  const selectJob = useCallback((job: JobDto) => {
    if (currentJob?.job_id === job.job_id) return;
    const targetIndex = targetForSelection(compareRepository, job);
    const restored = sessionForSelection(compareRepository, job, targetIndex);
    selectionRef.current = { job, targetIndex };
    setCurrentJob(job);
    setSelectedTargetIndex(targetIndex);
    resetResultWorkspace();
    setStatus(restored
      ? `${job.name} · restored ${restored.plan.ops.length} compare items`
      : `${job.name} · ${job.mode}${job.rigor !== 'standard' ? ` · ${job.rigor}` : ''}`);
    requestResultRestore(job, targetIndex, restored);
  }, [currentJob?.job_id, compareRepository, requestResultRestore, resetResultWorkspace, setStatus]);

  /// Write a root edited on the main screen back to the job TOML. Changing a root invalidates the current
  /// plan, so clear it too. For multi-target jobs, only the currently selected target changes.
  const saveRoot = useCallback(async (which: 'source' | 'target', value: string) => {
    if (!currentJob) return;
    const name = currentJob.name;
    const normalizedValue = value.trim();
    if (!normalizedValue) return;
    let detail: ipc.JobDetailDto;
    try {
      detail = await ipc.getJob(name);
    } catch (error) {
      await reportMutationFailure(name, `Failed to read the job before changing ${which}`, error);
      return;
    }
    const jobConfiguration = detail.job;
    const hasTargetList = jobConfiguration.targets.length > 0;
    const targets = hasTargetList ? [...jobConfiguration.targets] : [jobConfiguration.target];
    const before = which === 'source' ? jobConfiguration.source : targets[selectedTargetIndex];
    if (before === normalizedValue) return; // unchanged means no disk write — otherwise every blur would bump the mtime
    const next: ipc.JobFull = which === 'source'
      ? { ...jobConfiguration, source: normalizedValue }
      : {
        ...jobConfiguration,
        target: selectedTargetIndex === 0 ? normalizedValue : jobConfiguration.target,
        targets: hasTargetList
          ? targets.map((target, index) => (index === selectedTargetIndex ? normalizedValue : target))
          : [],
      };
    let saved: ipc.JobSaveDto;
    try {
      saved = await ipc.saveJob(name, next, {
        originalName: detail.name,
        expectedRevision: detail.config_revision,
      });
    } catch (error) {
      await reportMutationFailure(name, `Failed to write ${which} back to the job`, error);
      return;
    }
    // The mutation is committed at this point. Retire its old compare immediately, and never let a
    // later list-refresh failure make the UI claim that this successful disk write failed.
    setCompareRepository((repository) => reconcileSavedJobSession(
      repository,
      saved,
      { jobId: detail.job_id, name: detail.name, configRevision: detail.config_revision },
    ));
    resetResultWorkspace();
    pushHistory(normalizedValue);
    const undo = async () => {
      const back: ipc.JobFull = which === 'source'
        ? { ...next, source: before }
        : {
          ...next,
          target: selectedTargetIndex === 0 ? before : next.target,
          targets: next.targets.map((target, index) => (index === selectedTargetIndex ? before : target)),
        };
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
      } catch (error) {
        await reportMutationFailure(saved.name, `Could not restore ${which}`, error);
        return;
      }
      resetResultWorkspace();
      try {
        await refreshJobs(saved.name);
        setStatus(`Restored ${which}`);
      } catch (error) {
        setStatus(`Restored ${which}, but refreshing the job list failed: ${error}`, 'err');
      }
    };
    const success = `Changed ${which} → ${normalizedValue} — Compare again (Ctrl+R)`;
    try {
      await refreshJobs(name);
      setStatusUndo(success, 'Undo', undo);
    } catch (error) {
      setStatusUndo(`${success}; refreshing the job list failed: ${error}`, 'Undo', undo, 'err');
    }
  }, [currentJob, selectedTargetIndex, pushHistory, refreshJobs, reportMutationFailure, resetResultWorkspace, setStatus, setStatusUndo]);

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
      if (selectedTargetIndex >= targets.length) {
        setStatus(`Target ${selectedTargetIndex + 1} no longer exists — refresh the job before swapping`, 'err');
        return;
      }
      setAskSwap({
        jobId: detail.job_id,
        name: detail.name,
        job,
        configRevision: detail.config_revision,
        targetIndex: selectedTargetIndex,
      });
    } catch (error) {
      await reportMutationFailure(currentJob.name, 'Failed to read job before swapping', error);
    }
  }, [currentJob, busy, selectedTargetIndex, reportMutationFailure, setStatus]);

  const doSwap = useCallback(async (
    jobId: string,
    name: string,
    jobConfiguration: ipc.JobFull,
    configRevision: string,
    targetIndex: number,
  ) => {
    const targets = jobConfiguration.targets.length
      ? [...jobConfiguration.targets]
      : [jobConfiguration.target];
    const hasTargetList = jobConfiguration.targets.length > 0;
    const selectedTarget = targets[targetIndex];
    if (selectedTarget === undefined) {
      setStatus(`Swap failed: target ${targetIndex + 1} no longer exists`, 'err');
      return;
    }
    const nextTargets = targets.map((target, index) => (
      index === targetIndex ? jobConfiguration.source : target
    ));
    const next: ipc.JobFull = {
      ...jobConfiguration,
      source: selectedTarget,
      target: targetIndex === 0 ? jobConfiguration.source : jobConfiguration.target,
      targets: hasTargetList ? nextTargets : [],
    };
    let saved: ipc.JobSaveDto;
    try {
      saved = await ipc.saveJob(name, next, {
        originalName: name,
        expectedRevision: configRevision,
      });
    } catch (error) {
      await reportMutationFailure(name, 'Swap failed', error);
      return;
    }
    setCompareRepository((repository) => reconcileSavedJobSession(
      repository,
      saved,
      { jobId, name, configRevision },
    ));
    resetResultWorkspace();
    const undo = async () => {
      try {
        const restored = await ipc.saveJob(saved.name, jobConfiguration, {
          originalName: saved.name,
          expectedRevision: saved.config_revision,
        });
        setCompareRepository((repository) => reconcileSavedJobSession(
          repository,
          restored,
          { jobId: saved.job_id, name: saved.name, configRevision: saved.config_revision },
        ));
      } catch (error) {
        await reportMutationFailure(saved.name, 'Could not undo the root swap', error);
        return;
      }
      resetResultWorkspace();
      try {
        await refreshJobs(saved.name);
        setStatus(`Restored the two roots of '${saved.name}'`);
      } catch (error) {
        setStatus(`Restored the two roots of '${saved.name}', but refreshing the job list failed: ${error}`, 'err');
      }
    };
    const success = `Swapped the two roots of '${name}' — Compare again (Ctrl+R)`;
    try {
      await refreshJobs(name);
      setStatusUndo(success, 'Undo swap', undo);
    } catch (error) {
      setStatusUndo(`${success}; refreshing the job list failed: ${error}`, 'Undo swap', undo, 'err');
    }
  }, [refreshJobs, reportMutationFailure, resetResultWorkspace, setStatus, setStatusUndo]);

  /// Write an exclude back into the job's exclude list. Pruning during the scan only takes effect at the
  /// next Compare, so the message has to say so and leave an undo behind.
  const addExcludes = useCallback(async (masks: string[], label: string) => {
    if (!currentJob) { setStatus('Select a job first', 'err'); return; }
    const name = currentJob.name;
    let detail: ipc.JobDetailDto;
    try {
      detail = await ipc.getJob(name);
    } catch (error) {
      await reportMutationFailure(name, 'Failed to read the job before adding the exclude', error);
      return;
    }
    const jobConfiguration = detail.job;
    const previousExcludes = [...jobConfiguration.exclude];
    // Folded the way the engine folds, not by string equality: a mask typed with backslashes, in a
    // different case, or in NFD is the same rule to the filter, and appending it again would leave
    // two lines that mean one thing — and a preset box unticked next to its own pattern
    const { next: nextExcludes, added: addedMasks } = addExcludeEntries(previousExcludes, masks);
    if (!addedMasks.length) {
      setStatus(`The job already has ${masks.length > 1 ? 'all of these masks' : 'this exclude'}`);
      return;
    }
    const updatedJob = { ...jobConfiguration, exclude: nextExcludes };
    let saved: ipc.JobSaveDto;
    try {
      saved = await ipc.saveJob(name, updatedJob, {
        originalName: detail.name,
        expectedRevision: detail.config_revision,
      });
    } catch (error) {
      await reportMutationFailure(name, 'Failed to write exclude', error);
      return;
    }
    setCompareRepository((repository) => reconcileSavedJobSession(
      repository,
      saved,
      { jobId: detail.job_id, name: detail.name, configRevision: detail.config_revision },
    ));
    resetResultWorkspace();
    const undo = async () => {
      try {
        const restored = await ipc.saveJob(saved.name, jobConfiguration, {
          originalName: saved.name,
          expectedRevision: saved.config_revision,
        });
        setCompareRepository((repository) => reconcileSavedJobSession(
          repository,
          restored,
          { jobId: saved.job_id, name: saved.name, configRevision: saved.config_revision },
        ));
      } catch (error) {
        await reportMutationFailure(saved.name, 'Could not undo the exclude', error);
        return;
      }
      resetResultWorkspace();
      try {
        await refreshJobs(saved.name);
        setStatus('Exclude undone');
      } catch (error) {
        setStatus(`Exclude undone, but refreshing the job list failed: ${error}`, 'err');
      }
    };
    const success = `${label}: ${addedMasks.join(', ')} — Compare again to build a result with this exclusion`;
    try {
      await refreshJobs(name);
      setStatusUndo(success, 'Undo exclude', undo);
    } catch (error) {
      setStatusUndo(`${success}; refreshing the job list failed: ${error}`, 'Undo exclude', undo, 'err');
    }
  }, [currentJob, refreshJobs, reportMutationFailure, resetResultWorkspace, setStatus, setStatusUndo]);

  // CSV is a presentation snapshot, while Rust remains the single owner of escaping and the BOM.
  const exportCsv = useCallback(async () => {
    if (!plan || !currentJob) { setStatus('Compare first', 'err'); return; }
    if (resultView !== 'differences') { setStatus('Switch to Differences before exporting', 'err'); return; }
    if (scopeCalculationFailed) { setStatus('The run scope could not be calculated safely', 'err'); return; }
    if (scopeCalculationPending) { setStatus('The run scope is still being calculated', 'err'); return; }
    if (!inScopeIndices.length) { setStatus('The current run scope is empty', 'err'); return; }
    const stamp = new Date();
    const defaultFilename = `${currentJob.name}-${stamp.getFullYear()}${p2(stamp.getMonth() + 1)}${p2(stamp.getDate())}.csv`;
    try {
      const path = await ipc.pickPath({ save: true, title: 'Export CSV', defaultPath: defaultFilename });
      if (!path) return;
      // CSV is a snapshot of the presentation, so it deliberately follows display order rather
      // than the engine order used for execution.
      const exportedRowCount = await ipc.exportCsv(
        path, plan.header,
        layout.displayOrder.map((index) => effectiveOperation(plan, flipped, index)),
        layout.displayOrder.map((index) => rowMetadata(plan, index)),
        layout.displayOrder.map((index) => checked[index]),
      );
      setStatusUndo(`Exported ${exportedRowCount} rows to ${path}`, 'Open containing folder', () => ipc.reveal(path));
    } catch (error) {
      setStatus(`Export failed: ${error}`, 'err');
    }
  }, [plan, currentJob, resultView, scopeCalculationFailed, scopeCalculationPending, inScopeIndices, layout, flipped, checked, setStatus, setStatusUndo]);

  const browseRoot = useCallback(async (which: 'source' | 'target') => {
    try {
      const selectedPath = await ipc.pickPath({
        directory: true,
        title: `Select the ${which} directory`,
        defaultPath: which === 'source'
          ? currentJob?.source
          : currentJob?.targets[selectedTargetIndex] ?? currentJob?.target,
      });
      if (selectedPath) await saveRoot(which, selectedPath);
    } catch (error) {
      setStatus(`Can't open the picker: ${error}`, 'err');
    }
  }, [currentJob, selectedTargetIndex, saveRoot, setStatus]);

  const toggleRow = useCallback((index: number, value: boolean) => {
    setChecked((previous) => { const next = [...previous]; next[index] = value; return next; });
  }, [setChecked]);

  const toggleMany = useCallback((indices: number[], value: boolean) => {
    setChecked((previous) => {
      const next = [...previous];
      for (const index of indices) next[index] = value;
      return next;
    });
  }, [setChecked]);

  const flipRow = useCallback((index: number) => {
    setFlipped((previous) => {
      const next = [...previous];
      next[index] = !next[index];
      return next;
    });
  }, [setFlipped]);

  const toggleFolderFold = useCallback((folderPath: string) => {
    setCollapsedFolderPaths((previous) => {
      const next = new Set(previous);
      if (next.has(folderPath)) next.delete(folderPath); else next.add(folderPath);
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

  const rowMenu = useCallback((index: number, x: number, y: number) => {
    if (!plan) return;
    const operation = effectiveOperation(plan, flipped, index);
    const [sourcePath, targetPath] = sidePaths(operation);
    const sourceAbsolutePath = sourcePath ? fullPath(plan.header.source_root, sourcePath) : null;
    const targetAbsolutePath = targetPath ? fullPath(plan.header.target_root, targetPath) : null;
    const relativePath = operation.path;
    const baseName = baseOf(relativePath);
    const extensionSeparator = baseName.lastIndexOf('.');
    const extension = extensionSeparator > 0 ? baseName.slice(extensionSeparator + 1) : '';
    const folderPath = owningFolderOf(operation);
    const inFolderScope = inScopeIndices.filter((candidateIndex) => (
      matchesFolderScope(effectiveOperation(plan, flipped, candidateIndex), folderPath)
      && isExecutableOperation(effectiveOperation(plan, flipped, candidateIndex))
    ));
    const copyPath = (path: string) => navigator.clipboard?.writeText(path).then(
      () => setStatus(`Copied: ${path}`),
      () => setStatus('Copy failed (clipboard unavailable)', 'err'),
    );
    setContextMenu({
      x, y,
      entries: [
        { label: 'Show in Explorer · Source', disabled: !sourceAbsolutePath, run: () => { ipc.reveal(sourceAbsolutePath!).catch((error) => setStatus(String(error), 'err')); } },
        { label: 'Show in Explorer · Target', disabled: !targetAbsolutePath, run: () => { ipc.reveal(targetAbsolutePath!).catch((error) => setStatus(String(error), 'err')); } },
        { separator: true, label: '' },
        { label: 'Copy Full Path', run: () => copyPath((sourceAbsolutePath ?? targetAbsolutePath)!) },
        { label: 'Copy Relative Path', run: () => copyPath(relativePath) },
        { separator: true, label: '' },
        { label: extension ? `Exclude This Type */*.${extension}` : 'Exclude This Type (No Extension)', disabled: !extension || !currentJob, run: () => addExcludes([`*/*.${extension}`], 'Added to exclude') },
        { label: folderPath ? `Exclude This Directory /${folderPath}/` : 'Exclude This Directory (Already at the Root)', disabled: !folderPath || !currentJob, run: () => addExcludes([`/${folderPath}/`], 'Added to exclude') },
        { separator: true, label: '' },
        { label: flipped[index] ? 'Restore Original Direction' : 'Reverse This Row', disabled: !canReverseOperation(plan, index), run: () => flipRow(index) },
        { label: 'Check Only This Item', run: () => setChecked(plan.ops.map((_, candidateIndex) => candidateIndex === index && isExecutableOperation(effectiveOperation(plan, flipped, candidateIndex)))) },
        { label: `${folderPath ? 'Uncheck This Folder and Subfolders' : 'Uncheck Root-Level Items'} (${inFolderScope.length})`, disabled: inFolderScope.length === 0, run: () => toggleMany(inFolderScope, false) },
      ],
    });
  }, [plan, flipped, inScopeIndices, currentJob, addExcludes, flipRow, toggleMany, setChecked, setStatus]);

  useEffect(() => {
    (async () => {
      try {
        const list = await refreshJobs();
        refreshLastSyncs();
        setJobsDir(await ipc.jobsDir());
        try { setAppVersion('v' + (await getVersion())); } catch { /* ignore when the permission isn't granted */ }
        setStatus(list.length ? 'Select a job on the left to start' : 'No jobs — drop a <name>.toml into the jobs directory');
      } catch (error) {
        setStatus(`Init failed: ${error}`, 'err');
      }
    })();
  }, []);

  useEffect(() => {
    let disposed = false;
    let dispose: (() => void) | undefined;
    let ready = false;
    let lastSequence = 0;
    const queued: CompareProgressEvent[] = [];
    const handle = (event: CompareProgressEvent) => {
      if (event.purpose !== 'compare') return;
      if (!compareRunReady.current && event.run_id <= compareRunFloor.current) return;
      if (event.run_id < compareRunId.current) return;
      if (event.run_id > compareRunId.current) {
        compareRunId.current = event.run_id;
        compareRateByPhase.current.clear();
        setCompareStages([]);
      }
      compareRunReady.current = true;
      // A `log` event carries no phase, so the phase guard below would drop it. Errors do carry
      // one, but the guard used to sit above every branch and there was no branch to reach:
      // `phase_start` and `progress` were the only two, and a scan that could not read a directory
      // produced an event nothing was listening for.
      if (event.kind === 'error') {
        setStatus(`${event.action === 'walk' ? 'Scan could not read' : 'Error'}: ${event.message ?? ''}`, 'err');
        return;
      }
      if (!event.phase) return;
      if (event.kind === 'phase_start') {
        setCompareStages((previous) => reduceCompareStages(previous, event));
      } else if (event.kind === 'totals') {
        const timestampMs = event.ts_ms ?? Date.now();
        const bytesDone = event.bytes_done ?? 0;
        compareRateByPhase.current.set(event.phase, { timestampMs, bytesDone, smoothedRate: 0 });
        setCompareStages((previous) => reduceCompareStages(previous, event));
      } else if (event.kind === 'progress') {
        const timestampMs = event.ts_ms ?? Date.now();
        const bytesDone = event.bytes_done ?? 0;
        const previousRate = compareRateByPhase.current.get(event.phase);
        let smoothedRate = 0;
        if (previousRate
          && timestampMs > previousRate.timestampMs
          && bytesDone >= previousRate.bytesDone) {
          const instantaneousRate = ((bytesDone - previousRate.bytesDone) * 1000)
            / (timestampMs - previousRate.timestampMs);
          smoothedRate = previousRate.smoothedRate > 0
            ? previousRate.smoothedRate * 0.7 + instantaneousRate * 0.3
            : instantaneousRate;
          compareRateByPhase.current.set(event.phase, { timestampMs, bytesDone, smoothedRate });
        } else if (!previousRate) {
          compareRateByPhase.current.set(event.phase, { timestampMs, bytesDone, smoothedRate: 0 });
        } else {
          smoothedRate = previousRate.smoothedRate;
        }
        setCompareStages((previous) => reduceCompareStages(previous, event, smoothedRate));
      } else if (event.kind === 'phase_end') {
        setCompareStages((previous) => reduceCompareStages(previous, event));
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
    const pendingUnlisten = listen<string>('main-close-blocked', (event) => {
      setStatus(event.payload, 'err');
    });
    return () => { pendingUnlisten.then((dispose) => dispose()); };
  }, [setStatus]);

  // Tauri v2 has dragDropEnabled on by default, so HTML5 drop events never reach the webview — you must
  // go through onDragDropEvent, and payload.position is in **physical pixels**, to be converted yourself.
  useEffect(() => {
    let disposed = false;
    let dispose: (() => void) | undefined;
    getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload as unknown as {
        type: string;
        paths?: string[];
        position?: { x: number; y: number };
      };
      if (payload.type === 'leave') { setDropTargetKey(null); return; }
      const position = payload.position;
      if (!position) return;
      const pixelRatio = window.devicePixelRatio || 1;
      const x = position.x / pixelRatio;
      const y = position.y / pixelRatio;
      // While the editor is open only its fields count; otherwise the two roots on the main screen do.
      // Each region registers itself through a callback ref, so the editor's entry clears itself on
      // unmount and this stays a plain null check rather than a lookup that has to guess at markup.
      const scope = dropScope.current.editor ?? dropScope.current.path;
      const input = [...(scope?.querySelectorAll<HTMLInputElement>('input[data-drop]') ?? [])]
        .filter((candidate) => !candidate.disabled)
        .find((candidate) => {
          const bounds = candidate.getBoundingClientRect();
          return x >= bounds.left && x <= bounds.right && y >= bounds.top && y <= bounds.bottom;
        });
      const key = input?.dataset.root ?? input?.dataset.fieldKey ?? null;
      if (payload.type === 'over' || payload.type === 'enter') { setDropTargetKey(key); return; }
      if (payload.type !== 'drop') return;
      setDropTargetKey(null);
      const firstPath = payload.paths?.[0];
      if (!input || !key || !firstPath) return;
      void (async () => {
        // If a file was dropped, take its parent directory — a root field wants a directory
        let pathValue = firstPath;
        try {
          const pathInformation = await ipc.inspectPaths(firstPath, '');
          if (pathInformation.source.readiness === 'not_directory') {
            const separatorIndex = Math.max(pathValue.lastIndexOf('\\'), pathValue.lastIndexOf('/'));
            if (separatorIndex > 0) pathValue = pathValue.slice(0, separatorIndex);
          }
        } catch { /* if we can't tell, fill it in as-is */ }
        pushHistory(pathValue);
        // Dropping on the two main-screen roots edits the job right away (same path as typing and
        // pressing Enter); in the editor it waits for save
        if (input.dataset.root === 'source' || input.dataset.root === 'target') {
          await saveRoot(input.dataset.root as 'source' | 'target', pathValue);
        } else {
          editorApi.current?.setField(key, pathValue);
          setStatus(`Filled in: ${pathValue}`);
        }
      })();
    })
      // The unlisten handle can arrive after the effect has already been cleaned up (StrictMode
      // double-mounts in development); dropping it there would leak a second live handler
      .then((unlisten) => { if (disposed) unlisten(); else dispose = unlisten; })
      .catch(() => { /* if drag and drop is unavailable, typed paths still work */ });
    return () => { disposed = true; dispose?.(); };
  }, [pushHistory, saveRoot, setStatus]);

  useEffect(() => {
    if (!currentJob) { setJobConfiguration(null); return; }
    let live = true;
    ipc.getJob(currentJob.name).then((detail) => {
      if (live) setJobConfiguration(detail.job);
    }).catch((error) => {
      if (!live) return;
      setJobConfiguration(null);
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
          await ipc.completeAutoScan(ticket.generation, ticket.ticketId, false),
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
              const released = await ipc.completeAutoScan(ticket.generation, ticket.ticketId, false);
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

    if (compareInFlight.current || applyExecutionRequest.current || editor || applyReviewRequest.current || compareReviewRequest.current) {
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
        listen<AutoScanStatusDto>('autoscan-status', ({ payload }) => {
          if (disposed) return;
          const accepted = acceptAutoScanStatus(payload, 'event');
          if (accepted) setStatus(`AutoScan: ${payload.detail}`, payload.active ? '' : 'err');
        }),
        listen<AutoScanTriggerDto>('autoscan-trigger', ({ payload }) => {
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

  // Escape is deliberately absent: every overlay closes itself, and they stack, so one Escape
  // unwinds exactly one layer. Handling it here as well would close the sheet *and* the dialog
  // nested inside it on a single press.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const modifierPressed = event.ctrlKey || event.metaKey;
      // While an overlay owns the screen it owns the keyboard: F5 must not kick off a compare
      // behind an open editor
      if (contextMenu || editor || settingsOpen || askSwap) return;
      if (confirmOpen || compareReview.phase !== 'idle') return;
      // F5 / F9 = the FFS compare / synchronize keys; Ctrl+R also compares
      if (event.key === 'F5') { event.preventDefault(); void doCompare(); }
      else if (event.key === 'F9') { event.preventDefault(); void openConfirm(); }
      else if (modifierPressed && event.key.toLowerCase() === 'r') {
        event.preventDefault();
        void doCompare();
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [contextMenu, editor, settingsOpen, askSwap, confirmOpen, compareReview.phase, doCompare, openConfirm]);

  const hasDifferences = !!plan && plan.ops.length > 0;
  const reviewBusy = operationReviewPending(compareReview) || operationReviewPending(applyReview);
  const confirmTotals: ApplyReviewTotals | null = useMemo(() => {
    if (!plan) return null;
    const totals: ApplyReviewTotals = {
      copyCount: 0,
      updateCount: 0,
      moveCount: 0,
      deleteCount: 0,
      transferBytes: 0,
      deletionBytes: 0,
      reversedCount: 0,
      checkedOutsideScope: 0,
    };
    for (const index of executableIndices) {
      const operation = effectiveOperation(plan, flipped, index);
      if (operation.action === 'copy') {
        totals.copyCount++;
        totals.transferBytes += rowTransferBytes(plan, flipped, index);
      } else if (operation.action === 'update') {
        totals.updateCount++;
        totals.transferBytes += rowTransferBytes(plan, flipped, index);
      } else if (operation.action === 'chmod') totals.updateCount++;
      else if (operation.action === 'move') totals.moveCount++;
      else if (operation.action === 'delete' || operation.action === 'delete_dir') {
        totals.deleteCount++;
        totals.deletionBytes += operation.size ?? 0;
      }
      if (flipped[index]) totals.reversedCount++;
    }
    totals.checkedOutsideScope = checked.filter(Boolean).length - executableIndices.length;
    return totals;
  }, [plan, executableIndices, flipped, checked]);

  return (
    <>
      <div className="app">
        <Sidebar
          jobs={jobs}
          currentJobId={currentJob?.job_id ?? null}
          lastSyncByJobName={lastSyncByJobName}
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
            executableCount={executableIndices.length}
            stats={stats}
            busy={busy || reviewBusy}
            canSync={applyAvailability.available}
            applyBlockedMessage={applyAvailability.blockedMessage}
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
              const monitoredTarget = selectedTargetIndex;
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
            jobConfiguration={jobConfiguration}
            busy={busy}
            reviewing={reviewBusy}
            selectedTargetIndex={selectedTargetIndex}
            pathHistory={pathHistory}
            dropTargetKey={dropTargetKey === 'source' || dropTargetKey === 'target' ? dropTargetKey : null}
            scopeRef={setPathScope}
            onCommit={(which, v) => void saveRoot(which, v)}
            onBrowse={(which) => void browseRoot(which)}
            onSwap={() => void requestSwap()}
            onSelectTarget={(targetIndex) => {
              if (busy || targetIndex === selectedTargetIndex) return;
              const selected = currentJob;
              if (!selected) return;
              selectionRef.current = { job: selected, targetIndex };
              setSelectedTargetIndex(targetIndex);
              resetResultWorkspace();
              const targetPath = selected.targets[targetIndex] ?? '';
              const restored = sessionForSelection(compareRepository, selected, targetIndex);
              setStatus(restored
                ? `Switched target → ${targetPath} · restored ${restored.plan.ops.length} compare items`
                : `Switched target → ${targetPath} — Compare again (Ctrl+R)`);
              requestResultRestore(selected, targetIndex, restored);
            }}
            onEditGroup={(g) => { if (currentJob && !busy) setEditor({ name: currentJob.name, focusGroup: g }); }}
          />
          {plan && !compareActive && (
            <ResultBar
              plan={plan}
              resultView={resultView}
              onResultViewChange={(next) => {
                setResultView(next);
                if (next === 'identical') setAdvancedFiltersAnchor(null);
              }}
              searchDraft={searchDraft}
              searchPending={searchPending}
              scopeCalculationPending={scopeCalculationPending}
              scopeCalculationFailed={scopeCalculationFailed}
              onSearchDraftChange={setSearchDraft}
              onClearSearch={clearSearch}
              scope={{
                foundCount: plan.ops.length,
                inScopeCount: inScopeIndices.length,
                selectedCount: executableIndices.length,
                folderScope,
                selectedResultTypes: [...selectedResultTypes],
                advancedFilterCount: countActiveAdvancedFilterGroups(advancedFilter),
              }}
              onClearScope={clearRunScope}
              onClearFolderScope={() => setFolderScope(null)}
              onClearSelectedResultTypes={() => setSelectedResultTypes(new Set())}
              onClearAdvancedFilters={clearAdvancedFilters}
              advancedFiltersOpen={!!advancedFiltersAnchor}
              onToggleAdvancedFilters={(anchor) => setAdvancedFiltersAnchor((current) => (current ? null : anchor))}
              onExportCsv={() => void exportCsv()}
              grouped={grouped}
              sort={sort}
              anyCollapsed={anyCollapsed}
              pathMode={pathMode}
              onToggleFold={() => setCollapsedFolderPaths(anyCollapsed ? new Set() : new Set(folderPathsInLayout))}
              // Grouping and sorting are independent now — a sort orders rows inside each group and
              // the groups among themselves, so this button no longer has to double as a sort clear
              onToggleGroup={() => {
                const next = !grouped;
                setGrouped(next);
                localStorage.setItem('sd.grouped', next ? 'on' : 'off');
                setCollapsedFolderPaths(new Set());
              }}
              onClearSort={() => setSort(null)}
              onTogglePathMode={() => {
                const next = pathMode === 'rel' ? 'full' : 'rel';
                setPathMode(next);
                localStorage.setItem('sd.pathmode', next);
              }}
              resultPanelId={resultPanelId}
              differencesTabId={differencesTabId}
              identicalTabId={identicalTabId}
            />
          )}
          {plan && <ScanFaultBanner header={plan.header} />}
          <div className="results-layout">
            {hasDifferences && resultView === 'differences' && !compareActive && (
              <RunScopePanel
                plan={plan}
                flipped={flipped}
                selectedResultTypes={selectedResultTypes}
                onSelectedResultTypesChange={setSelectedResultTypes}
                folderScope={folderScope}
                onFolderScopeChange={setFolderScope}
                collapsed={panelCollapsed}
                expandedFolders={expandedFolders}
                onToggleCollapsed={togglePanelCollapsed}
                onToggleExpandedFolder={toggleExpandedFolder}
                onClearRunScope={clearRunScope}
              />
            )}
            {/* A callback ref into state, not a useRef: the table measures this element, and a child's
                effects run before an ancestor host ref attaches — so on mount a ref would still be
                null exactly when the virtual window first needs it */}
            <div
              id={resultPanelId}
              className="results-viewport"
              role={plan && !compareActive ? 'tabpanel' : undefined}
              aria-labelledby={plan && !compareActive
                ? (resultView === 'identical' ? identicalTabId : differencesTabId)
                : undefined}
              ref={setResultsViewport}
            >
              {compareActive ? (
                <ComparePanel
                  stages={compareStages}
                  cancelling={compareCancelling}
                  onCancel={() => {
                    if (!compareRunReady.current || compareRunId.current < 0) {
                      setStatus('Compare is still starting — cancel will be available when its run is registered');
                      return;
                    }
                    const runId = compareRunId.current;
                    setCompareCancelling(true);
                    setStatus('Cancelling the compare…');
                    ipc.cancelRun(runId).then((accepted) => {
                      if (accepted) return;
                      setCompareCancelling(false);
                      setStatus('That compare already finished; no newer run was cancelled');
                    }).catch((e) => {
                      setCompareCancelling(false);
                      setStatus(`Cancel failed: ${e}`, 'err');
                    });
                  }}
                />
              ) : resultView === 'identical' && plan ? (
                <IdenticalResultsPanel owner={plan.owner} />
              ) : hasDifferences ? (
                <PlanTable
                  plan={plan}
                  flipped={flipped}
                  checked={checked}
                  rowPlan={rowPlan}
                  displayOrder={layout.displayOrder}
                  inScopeIndices={inScopeIndices}
                  pathMode={pathMode}
                  grouped={grouped}
                  sort={sort}
                  collapsedFolderPaths={collapsedFolderPaths}
                  wrap={resultsViewport}
                  onToggleRow={toggleRow}
                  onToggleMany={toggleMany}
                  onFlip={flipRow}
                  onToggleFolderFold={toggleFolderFold}
                  onSort={toggleSort}
                  onContextRow={rowMenu}
                />
              ) : plan ? (
                <Placeholder
                  icon={<CircleCheck size={26} className="icon-ok" />}
                  title="No Differences in the Compared Scope"
                  description="No synchronization actions are planned. Identical lists matching files; the status bar preserves excluded and unread counts."
                />
              ) : (
                <Placeholder
                  icon={<FolderSearch size={26} />}
                  title={currentJob ? `Ready — ${currentJob.name}` : 'No Job Selected'}
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
            inScopeCount={inScopeIndices.length}
            resultView={resultView}
            zoom={zoom.zoom}
            onZoomIn={zoom.zoomIn}
            onZoomOut={zoom.zoomOut}
            onZoomReset={zoom.zoomReset}
          />
        </main>
      </div>

      {contextMenu && (
        <ContextMenu at={contextMenu} onClose={() => setContextMenu(null)}>
          {contextMenu.entries.map((entry, index) => (entry.separator
            ? <MenuDivider key={index} />
            : <MenuItem key={index} disabled={entry.disabled} danger={entry.danger} onClick={entry.run}>{entry.label}</MenuItem>
          ))}
        </ContextMenu>
      )}
      {advancedFiltersAnchor && plan && (
        <AdvancedFiltersPopover
          anchor={advancedFiltersAnchor}
          advancedFilter={advancedFilter}
          maskDraft={maskDraft}
          inScopeCount={inScopeIndices.length}
          differenceCount={plan.ops.length}
          onAdvancedFilterChange={setAdvancedFilter}
          onMaskDraftChange={setMaskDraft}
          onClear={clearAdvancedFilters}
          onWriteMasksToJob={(masks) => {
            if (!masks.length) { setStatus('Write at least one mask first', 'err'); return; }
            setAdvancedFiltersAnchor(null);
            void addExcludes(masks, 'Written into the exclude list');
          }}
          onClose={() => setAdvancedFiltersAnchor(null)}
        />
      )}
      {editor && (
        <JobEditor
          name={editor.name}
          focusGroup={editor.focusGroup}
          dropTargetKey={dropTargetKey}
          scopeRef={setEditorScope}
          apiRef={editorApi}
          busy={busy}
          onClose={() => setEditor(null)}
          onSaved={async (saved, job, original) => {
            const selectedIdentity = !!original && currentJob?.job_id === original.jobId;
            const semanticMutation = !!original
              && saved.config_revision !== original.configRevision;
            const preservesCompareResult = !!original
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
            if (selectedIdentity && !preservesCompareResult) {
              resetResultWorkspace();
            }
            pushHistory(job.source);
            pushHistory(job.target);
            try {
              await refreshJobs(selectedIdentity ? original?.name : undefined);
              if (selectedIdentity) setJobConfiguration(job);
              setStatus(
                saved.effect === 'no_op' ? `No changes to save for '${saved.name}'` : `Saved '${saved.name}'`,
                'ok',
              );
            } catch (error) {
              setStatus(`Saved '${saved.name}', but refreshing the job list failed: ${error}`, 'err');
            }
          }}
          onDeleted={async (deleted) => {
            setEditor(null);
            invalidateCompareJob(deleted.job_id);
            if (currentJob?.job_id === deleted.job_id) {
              resetResultWorkspace();
              setCurrentJob(null);
              setSelectedTargetIndex(0);
            }
            try {
              await refreshJobs();
              setStatus(`Deleted '${deleted.name}'`);
            } catch (error) {
              setStatus(`Deleted '${deleted.name}', but refreshing the job list failed: ${error}`, 'err');
            }
          }}
          onMutationConflict={async (name, original) => {
            const list = await refreshJobs(name);
            if (!original) return;
            const refreshed = list.find((candidate) => candidate.job_id === original.jobId) ?? null;
            setCompareRepository((repository) => reconcileRefreshedJobSession(repository, original, refreshed));
            if (currentJob?.job_id === original.jobId
              && (!refreshed || currentJob.config_revision !== refreshed.config_revision)) {
              resetResultWorkspace();
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
      {compareReview.phase !== 'idle' && compareReview.phase !== 'authorized' && (
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
