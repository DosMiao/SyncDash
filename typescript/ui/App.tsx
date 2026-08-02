// This orchestration shell owns session selection, compare/apply reviews, and mutating Tauri
// workflows. Result semantics live in core, effectful domain state in hooks/state, and rendering in
// components. Execution receives only a one-use authorization token; Rust owns the authenticated
// plan and reconstructs every operation.

import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useReducer, useRef, useState } from 'react';
import { CircleCheck, FolderSearch } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';
import { getVersion } from '@tauri-apps/api/app';
import { getCurrentWebview } from '@tauri-apps/api/webview';

import * as ipc from '../core/ipc';
import { owningFolderOf } from '../core/folders';
import { buildLayout, flattenLayout, layoutFolderPaths } from '../core/grouping';
import { addExcludeEntries } from '../core/junk';
import { joinDisplayPath, relativePathBaseName } from '../core/format';
import {
  canReverseOperation,
  effectiveOperation,
  keySpec,
  rowTransferBytes,
  isExecutableOperation,
  buildReviewedRowDecisions,
  sidePaths,
} from '../core/plan';
import {
  computeExecutableIndices,
  computeInScopeIndices,
  countActiveAdvancedFilterGroups,
  EMPTY_ADVANCED_SCOPE_FILTER,
  matchesFolderScope,
} from '../core/runScope';
import { reduceCompareStages } from '../core/compareProgress';
import type { PlanDto, ResultType, Sort, SortKey } from '../core/plan';
import type { CompareProgressEvent, CompareStage } from '../core/compareProgress';
import type { PlanLayout } from '../core/grouping';
import type { JobDto } from '../core/types/generated/JobDto';
import type { RunRecord } from '../core/types/generated/RunRecord';
import type { AutoScanStatusDto } from '../core/types/generated/AutoScanStatusDto';
import type { AutoScanTriggerDto } from '../core/types/generated/AutoScanTriggerDto';
import type { CompareScopeExecutionStatusDto } from '../core/types/generated/CompareScopeExecutionStatusDto';

import { useStatus } from './hooks/useStatus';
import { useCompareWorkspaceController } from './hooks/useCompareWorkspaceController';
import { useJobRegistryController } from './hooks/useJobRegistryController';
import { useOperationReviewExpiry } from './hooks/useOperationReviewExpiry';
import { useOwnedResultViewport } from './hooks/useOwnedResultViewport';
import { useZoomControl } from './hooks/useZoomControl';
import { useInteractionLayer } from './hooks/useInteractionLayer';
import {
  activeWorkspace,
  compareResultKey,
  compareScopeForJob,
  compareScopeKey,
  emptyCompareWorkspaceRepository,
  preferredTargetIndex,
  scopeWorkspace,
  workspaceHasReviewEdits,
} from './state/compareWorkspaceModel';
import type {
  CompareResultKey,
  CompareScopeKey,
  CompareWorkspacePreferences,
  DifferenceViewport,
  IdenticalViewport,
} from './state/compareWorkspaceModel';
import { deriveWorkspaceExecutionAccess } from './state/compareWorkspaceExecution';
import {
  exactWorkspaceLookupProblem,
  reduceCompareWorkspaces,
  scopeWorkspaceLookupProblem,
} from './state/compareWorkspaceRepository';
import {
  loadCompareWorkspacePreferences,
  saveCompareWorkspacePreferences,
} from './state/compareWorkspacePreferences';
import {
  AutoScanTicketLedger,
  autoScanToggleAction,
  monitorOwnsAutoScanResult,
  monitorOwnsAutoScanTicket,
  reconcileAutoScanStatus,
  statusCanOwnAutoScanTrigger,
  statusCompletesAutoScanTicket,
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
import {
  CompareActivityNotice,
  CompareCandidateNotice,
  CompareExecutionNotice,
} from './components/CompareWorkspaceNotices';
import { SettingsSheet } from './components/SettingsSheet';
import { Sidebar } from './components/Sidebar';
import { StatusBar } from './components/StatusBar';
import { Toolbar } from './components/Toolbar';
import { ConfirmDialog, ContextMenu, MenuDivider, MenuItem, Placeholder } from './components/ui';
import type { ApplyReviewTotals } from './components/ConfirmSheet';
import type { JobEditorApi } from './components/JobEditor';
import { deriveApplyAvailability } from './state/applyAvailability';
import {
  interactionBlocksUnattendedWrite,
  interactionConflictsWithReservedWrite,
} from './state/execution-safety';
import type { ExecutionInteractionState } from './state/execution-safety';
import { RequestFence } from './state/request-fence';
import {
  activeRootEditor,
  emptyRootEditorRepository,
  reduceRootEditors,
  rootDraftIsDirty,
} from './state/rootEditor';
import type {
  RootEditorKey,
  RootEditorOwner,
  RootEditorWorkspace,
  RootField,
  RootValues,
} from './state/rootEditor';
import { addPathToHistory, loadPathHistory, savePathHistory } from './state/pathHistory';

/// Stable identity for "no plan, nothing to lay out" — a fresh object literal here would make the
/// flatten memo below recompute on every render
const EMPTY_LAYOUT: PlanLayout = { displayOrder: [], folderTree: null };
const EMPTY_FLAGS: boolean[] = [];
const EMPTY_RESULT_TYPES = new Set<ResultType>();
const EMPTY_PATH_SET = new Set<string>();

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
interface JobIdentitySnapshot { jobId: string; name: string; configRevision: string }
interface CompareActivityRequest {
  scope: ReturnType<typeof compareScopeForJob>;
  requestId: number;
}
interface RootSwapRequest {
  workspaceKey: RootEditorKey;
  owner: RootEditorOwner;
  values: RootValues;
  mode: string;
}

function snapshotJobIdentity(job: JobDto): JobIdentitySnapshot {
  return { jobId: job.job_id, name: job.name, configRevision: job.config_revision };
}

function rootMutationState(
  result: ipc.JobRootMutationDto,
  targetIndex: number,
): { owner: RootEditorOwner; values: RootValues } {
  const target = result.targets[targetIndex];
  if (target === undefined) {
    throw new Error(`The root-mutation response omitted target ${targetIndex + 1}`);
  }
  return {
    owner: {
      jobId: result.mutation.job_id,
      jobName: result.mutation.name,
      configRevision: result.mutation.config_revision,
      targetIndex,
    },
    values: { source: result.source, target },
  };
}

function statusDeliveryWarning(mutation: { status_delivery_warnings: string[] }): string {
  return mutation.status_delivery_warnings.length
    ? ` · desktop status delivery warning: ${mutation.status_delivery_warnings.join('; ')}`
    : '';
}

export function App() {
  const {
    jobs,
    selectedJob,
    refresh: refreshJobs,
    select: setRegistrySelection,
  } = useJobRegistryController();
  const [jobConfiguration, setJobConfiguration] = useState<ipc.JobFull | null>(null);
  const [latestRunByJobId, setLatestRunByJobId] = useState<Record<string, RunRecord>>({});
  const latestRunSummaryFence = useRef(new RequestFence());
  const [appVersion, setAppVersion] = useState('');
  const [jobsDir, setJobsDir] = useState('');
  const [initialPathHistoryLoad] = useState(() => loadPathHistory(localStorage));
  const [pathHistory, setPathHistory] = useState<string[]>(initialPathHistoryLoad.paths);
  const pathHistoryRef = useRef(pathHistory);
  pathHistoryRef.current = pathHistory;
  const [selectedTargetIndex, setSelectedTargetIndex] = useState(0);
  const [rootEditorRepository, dispatchRootEditor] = useReducer(
    reduceRootEditors,
    emptyRootEditorRepository,
  );
  const selectedRootEditor = activeRootEditor(rootEditorRepository);
  const rootSaveRequestId = useRef(0);
  const rootPickerRequestId = useRef(0);
  const rootSaveInFlight = useRef<{ workspaceKey: RootEditorKey; requestId: number } | null>(null);
  const liveRootEditor = useRef<RootEditorWorkspace | null>(null);
  liveRootEditor.current = selectedRootEditor;
  const rootDraftOpen = !!selectedRootEditor && (
    rootDraftIsDirty(selectedRootEditor, 'source') || rootDraftIsDirty(selectedRootEditor, 'target')
  );

  const [compareWorkspaceRepository, dispatchCompareWorkspace] = useReducer(
    reduceCompareWorkspaces,
    emptyCompareWorkspaceRepository,
  );
  const [initialPreferenceLoad] = useState(() => loadCompareWorkspacePreferences(localStorage));
  const [workspacePreferences, setWorkspacePreferences] = useState<CompareWorkspacePreferences>(
    initialPreferenceLoad.preferences,
  );
  const restoreRequestId = useRef(0);
  const [busy, setBusy] = useState(false);
  const csvExportInFlight = useRef<CompareResultKey | null>(null);
  const [csvExportPending, setCsvExportPending] = useState(false);
  const applyExecutionRequest = useRef<ReviewRequestFence | null>(null);
  const autoApplyInFlight = useRef(false);

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
  useOperationReviewExpiry(applyReview, dispatchApplyReview);
  useOperationReviewExpiry(compareReview, dispatchCompareReview);
  const [advancedFiltersAnchor, setAdvancedFiltersAnchor] = useState<DOMRect | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [logOpen, setLogOpen] = useState(false);
  const [logReload, setLogReload] = useState(0);
  const [dropTargetKey, setDropTargetKey] = useState<string | null>(null);
  const [resultViewportElement, setResultViewportElement] = useState<HTMLDivElement | null>(null);
  const [candidateAdoption, setCandidateAdoption] = useState<{
    scopeKey: CompareScopeKey;
    resultKey: CompareResultKey;
  } | null>(null);
  const liveInteractionState = useRef<ExecutionInteractionState>({
    busy: false,
    editorOpen: false,
    settingsOpen: false,
    confirmationOpen: false,
    candidateAdoptionOpen: false,
    rootDraftOpen: false,
    rootSwapOpen: false,
    contextMenuOpen: false,
    reviewPending: false,
  });
  const [askSwap, setAskSwap] = useState<RootSwapRequest | null>(null);
  // The drag handler is registered once and must read the live droppable regions at drop time.
  const dropScope = useRef<{ editor: HTMLElement | null; path: HTMLElement | null }>({ editor: null, path: null });
  // Stable identities: a ref callback whose identity changes is detached with null and reattached
  // on every render, and these two are handed to components that re-render on every keystroke.
  const setPathScope = useCallback((element: HTMLElement | null) => { dropScope.current.path = element; }, []);
  const setEditorScope = useCallback((element: HTMLElement | null) => { dropScope.current.editor = element; }, []);

  const [compareActive, setCompareActive] = useState(false);
  const [compareStages, setCompareStages] = useState<CompareStage[]>([]);
  const [compareCancelling, setCompareCancelling] = useState(false);
  useLayoutEffect(() => {
    liveInteractionState.current = {
      busy,
      editorOpen: editor !== null,
      settingsOpen,
      confirmationOpen: confirmOpen,
      candidateAdoptionOpen: candidateAdoption !== null,
      rootDraftOpen,
      rootSwapOpen: askSwap !== null,
      contextMenuOpen: contextMenu !== null,
      reviewPending: operationReviewPending(compareReview) || operationReviewPending(applyReview),
    };
  }, [applyReview, askSwap, busy, candidateAdoption, compareReview, confirmOpen, contextMenu, editor, rootDraftOpen, settingsOpen]);
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
  const compareActivityRequestId = useRef(0);

  // AutoScan is backend-owned. The webview renders status and handles exact trigger tickets; it
  // never owns the clock or assumes that remaining mounted means the watcher is still alive.
  const [autoScanStatus, setAutoScanStatus] = useState<AutoScanStatusDto | null>(null);
  const autoScanStatusRef = useRef<AutoScanStatusDto | null>(null);
  const autoScanTicket = useRef<AutoScanTicket | null>(null);
  const autoScanLedger = useRef(new AutoScanTicketLedger());
  const autoScanControlRequest = useRef(0);
  const [autoScanControlPending, setAutoScanControlPending] = useState<'start' | 'stop' | null>(null);
  const autoScanControlPendingRef = useRef<'start' | 'stop' | null>(null);
  const autoScanTriggerRef = useRef<(trigger: AutoScanTriggerDto) => Promise<void>>(async () => {});

  const editorApi = useRef<JobEditorApi | null>(null);
  const {
    status,
    setMessage: setStatus,
    offerAction: setStatusAction,
    executeAction: runStatusAction,
    dismissNotice: dismissStatusNotice,
  } = useStatus('');
  const persistWorkspacePreferences = useCallback((preferences: CompareWorkspacePreferences) => {
    const warning = saveCompareWorkspacePreferences(localStorage, preferences);
    if (warning) setStatus(warning, 'err');
  }, [setStatus]);
  const reportZoomFailure = useCallback((message: string) => setStatus(message, 'err'), [setStatus]);
  const zoom = useZoomControl(reportZoomFailure);
  const selectionRef = useRef<{ job: JobDto | null; targetIndex: number }>({ job: null, targetIndex: 0 });
  selectionRef.current = { job: selectedJob, targetIndex: selectedTargetIndex };

  useEffect(() => {
    const warnings = [initialPreferenceLoad.warning, initialPathHistoryLoad.warning]
      .filter((warning): warning is string => warning !== null);
    if (warnings.length > 0) setStatus(warnings.join(' · '), 'err');
  }, [initialPathHistoryLoad.warning, initialPreferenceLoad.warning, setStatus]);

  useLayoutEffect(() => {
    const target = selectedJob?.targets[selectedTargetIndex];
    if (!selectedJob || target === undefined) {
      dispatchRootEditor({
        type: 'selection_rebound',
        owner: null,
        values: { source: '', target: '' },
      });
      return;
    }
    dispatchRootEditor({
      type: 'selection_rebound',
      owner: {
        jobId: selectedJob.job_id,
        jobName: selectedJob.name,
        configRevision: selectedJob.config_revision,
        targetIndex: selectedTargetIndex,
      },
      values: { source: selectedJob.source, target },
    });
  }, [selectedJob, selectedTargetIndex]);

  const selectedScopeWorkspace = selectedJob
    ? scopeWorkspace(compareWorkspaceRepository, compareScopeForJob(selectedJob, selectedTargetIndex))
    : null;
  const selectedCompareWorkspace = activeWorkspace(compareWorkspaceRepository, selectedJob, selectedTargetIndex);
  const selectedCompareWorkspaceKeyRef = useRef<CompareResultKey | null>(selectedCompareWorkspace?.key ?? null);
  selectedCompareWorkspaceKeyRef.current = selectedCompareWorkspace?.key ?? null;
  const plan = selectedCompareWorkspace?.plan ?? null;
  const differenceWorkspace = selectedCompareWorkspace?.differences ?? null;
  const includedRows = differenceWorkspace?.rowIncluded ?? EMPTY_FLAGS;
  const reversedRows = differenceWorkspace?.rowReversed ?? EMPTY_FLAGS;
  const selectedResultTypes = differenceWorkspace?.selectedResultTypes ?? EMPTY_RESULT_TYPES;
  const searchDraft = differenceWorkspace?.searchDraft ?? '';
  const searchQuery = differenceWorkspace?.appliedSearch ?? '';
  const folderScope = differenceWorkspace?.folderScope ?? null;
  const appliedAdvancedFilter = differenceWorkspace?.appliedAdvancedFilter ?? EMPTY_ADVANCED_SCOPE_FILTER;
  const expandedFolders = differenceWorkspace?.expandedScopeFolders ?? EMPTY_PATH_SET;
  const panelCollapsed = differenceWorkspace?.scopePanelCollapsed ?? workspacePreferences.scopePanelCollapsed;
  const sort = differenceWorkspace?.sort ?? null;
  const grouped = differenceWorkspace?.grouped ?? workspacePreferences.grouped;
  const pathMode = differenceWorkspace?.pathMode ?? workspacePreferences.pathMode;
  const collapsedFolderPaths = differenceWorkspace?.collapsedFolders ?? EMPTY_PATH_SET;
  const resultView = selectedCompareWorkspace?.selectedView ?? 'differences';
  const workspaceExecutionAccess = selectedCompareWorkspace
    ? deriveWorkspaceExecutionAccess(selectedCompareWorkspace, selectedScopeWorkspace?.execution ?? null)
    : null;
  const reviewEditable = workspaceExecutionAccess?.status === 'executable'
    && selectedScopeWorkspace?.activity.status === 'idle';
  const reportRunScopeError = useCallback((message: string) => setStatus(message, 'err'), [setStatus]);
  const workspaceController = useCompareWorkspaceController(
    selectedCompareWorkspace,
    dispatchCompareWorkspace,
    reportRunScopeError,
  );
  const recordIdenticalViewport = useCallback((resultKey: CompareResultKey, viewport: IdenticalViewport) => {
    dispatchCompareWorkspace({ type: 'identical_viewport_changed', resultKey, viewport });
  }, []);
  const recordDifferenceViewport = useCallback((resultKey: CompareResultKey, viewport: DifferenceViewport) => {
    dispatchCompareWorkspace({ type: 'difference_viewport_changed', resultKey, viewport });
  }, []);
  useOwnedResultViewport<CompareResultKey>(
    selectedCompareWorkspace?.selectedView === 'identical' ? resultViewportElement : null,
    selectedCompareWorkspace?.selectedView === 'identical' ? selectedCompareWorkspace.key : null,
    selectedCompareWorkspace?.identical.viewport ?? { scrollTop: 0, scrollLeft: 0 },
    recordIdenticalViewport,
  );
  const searchPending = differenceWorkspace !== null && searchDraft.trim() !== searchQuery;
  const maskStatus = differenceWorkspace?.maskResolution.status ?? 'not_required';
  const excludedByMask = useMemo(() => {
    if (!plan || appliedAdvancedFilter.masks.length === 0) return [];
    if (differenceWorkspace?.maskResolution.status === 'ready') {
      return differenceWorkspace.maskResolution.excludedByRow;
    }
    return plan.ops.map(() => true);
  }, [appliedAdvancedFilter.masks.length, differenceWorkspace?.maskResolution, plan]);
  const scopeCalculationPending = searchPending
    || maskStatus === 'unresolved'
    || maskStatus === 'pending';
  const scopeCalculationFailed = maskStatus === 'failed';
  const resultPanelId = useId();
  const differencesTabId = `${resultPanelId}-differences-tab`;
  const identicalTabId = `${resultPanelId}-identical-tab`;

  // Three memos, not one, because the three questions change at different rates. Membership is the
  // expensive full-table scan and no longer depends on `sort`, so clicking a header does not re-run
  // it; the layout does the sorting; flattening only decides which member rows a fold emits, so
  // folding one directory costs one pass instead of redoing the sort.
  //
  // `reversedRows` legitimately appears in all three: reversal changes the effective operation, its
  // owning folder, side paths, and sort key. It therefore requires a complete derived-state rebuild.
  const inScopeIndices = useMemo(() => (
    plan ? computeInScopeIndices({
      plan,
      reversedRows,
      selectedResultTypes,
      searchQuery,
      folderScope,
      advancedFilter: appliedAdvancedFilter,
      excludedByMask,
    }) : []
  ), [plan, reversedRows, selectedResultTypes, searchQuery, folderScope, appliedAdvancedFilter, excludedByMask]);

  const executableIndices = useMemo(() => (
    plan ? computeExecutableIndices(plan, reversedRows, inScopeIndices, includedRows) : []
  ), [plan, reversedRows, inScopeIndices, includedRows]);
  const reviewedRowDecisions = useMemo(
    () => (plan ? buildReviewedRowDecisions(executableIndices, reversedRows) : []),
    [plan, executableIndices, reversedRows],
  );
  const reviewKey = useMemo(() => (
    plan && selectedJob && workspaceExecutionAccess?.status === 'executable'
      ? applyReviewKey(
        plan.owner.identity,
        selectedJob.job_id,
        selectedJob.config_revision,
        selectedTargetIndex,
        workspaceExecutionAccess.verificationEpoch,
        reviewedRowDecisions,
      )
      : null
  ), [plan, selectedJob, selectedTargetIndex, workspaceExecutionAccess, reviewedRowDecisions]);
  const currentReviewKeyRef = useRef<string | null>(reviewKey);
  currentReviewKeyRef.current = reviewKey;
  const currentCompareReviewKey = useMemo(() => (
    selectedJob
      ? compareReviewKey(selectedJob.job_id, selectedJob.config_revision, selectedTargetIndex)
      : null
  ), [selectedJob, selectedTargetIndex]);
  const currentCompareReviewKeyRef = useRef<string | null>(currentCompareReviewKey);
  currentCompareReviewKeyRef.current = currentCompareReviewKey;

  const layout = useMemo(() => (
    plan ? buildLayout({ plan, reversedRows, inScopeIndices, grouped, sort }) : EMPTY_LAYOUT
  ), [plan, reversedRows, inScopeIndices, grouped, sort]);

  const rowPlan = useMemo(() => flattenLayout(layout, collapsedFolderPaths), [layout, collapsedFolderPaths]);
  const folderPathsInLayout = useMemo(() => layoutFolderPaths(layout), [layout]);
  // A filter may temporarily remove a collapsed branch. Only keys present in this layout decide
  // whether the toolbar says Expand all; otherwise one stale path leaves the control backwards.
  const anyCollapsed = useMemo(
    () => folderPathsInLayout.some((folderPath) => collapsedFolderPaths.has(folderPath)),
    [folderPathsInLayout, collapsedFolderPaths],
  );

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
      const operation = effectiveOperation(plan, reversedRows, index);
      switch (operation.action) {
        case 'copy': next.copyCount++; next.transferBytes += rowTransferBytes(plan, reversedRows, index); break;
        case 'update': next.updateCount++; next.transferBytes += rowTransferBytes(plan, reversedRows, index); break;
        case 'chmod': next.updateCount++; break;
        case 'move': next.moveCount++; break;
        case 'delete': case 'delete_dir': next.deleteCount++; break;
      }
      if (reversedRows[index]) next.reversedCount++;
    }
    return next;
  }, [plan, executableIndices, reversedRows]);

  const applyAvailability = useMemo(() => {
    return deriveApplyAvailability({
      workspace: selectedCompareWorkspace,
      workspaceExecutionAccess,
      compareActivity: selectedScopeWorkspace?.activity ?? null,
      scopeCalculationPending,
      scopeCalculationFailed,
      executableCount: executableIndices.length,
    });
  }, [
    selectedCompareWorkspace,
    workspaceExecutionAccess,
    selectedScopeWorkspace?.activity,
    scopeCalculationPending,
    scopeCalculationFailed,
    executableIndices.length,
  ]);

  const pushHistory = useCallback((candidatePath: string) => {
    const nextPaths = addPathToHistory(pathHistoryRef.current, candidatePath);
    if (nextPaths === pathHistoryRef.current) return;
    pathHistoryRef.current = nextPaths;
    setPathHistory(nextPaths);
    const warning = savePathHistory(localStorage, nextPaths);
    if (warning) setStatus(warning, 'err');
  }, [setStatus]);

  const reconcileWorkspaceJob = useCallback((
    previous: JobIdentitySnapshot,
    refreshedJob: JobDto | null,
  ) => {
    if (!refreshedJob || refreshedJob.job_id !== previous.jobId) {
      dispatchCompareWorkspace({
        type: 'job_execution_expired',
        jobId: previous.jobId,
        reason: 'job_deleted',
      });
    } else if (refreshedJob.config_revision !== previous.configRevision) {
      dispatchCompareWorkspace({
        type: 'job_execution_expired',
        jobId: previous.jobId,
        configRevision: previous.configRevision,
        reason: 'job_changed',
      });
    }
  }, []);

  const reconcileSavedWorkspaceJob = useCallback((
    saved: ipc.JobSaveDto,
    previous: JobIdentitySnapshot | null,
  ) => {
    if (!previous || saved.effect === 'created' || saved.effect === 'no_op') return;
    if (saved.job_id !== previous.jobId) {
      dispatchCompareWorkspace({
        type: 'job_execution_expired',
        jobId: previous.jobId,
        reason: 'job_deleted',
      });
    } else if (saved.config_revision !== previous.configRevision) {
      dispatchCompareWorkspace({
        type: 'job_execution_expired',
        jobId: previous.jobId,
        configRevision: previous.configRevision,
        reason: 'job_changed',
      });
    }
  }, []);

  const refreshLatestRunSummaries = useCallback(() => {
    const ticket = latestRunSummaryFence.current.start('latest-run-summaries');
    ipc.latestRunRecords().then(
      (latestRuns) => {
        if (latestRunSummaryFence.current.owns(ticket)) {
          setLatestRunByJobId(Object.fromEntries(
            latestRuns.map(({ job_id: jobId, record }) => [jobId, record]),
          ));
        }
      },
      (error: unknown) => {
        if (latestRunSummaryFence.current.owns(ticket)) {
          setStatus(`Could not refresh the latest-run indicators: ${error}`, 'err');
        }
      },
    );
  }, [setStatus]);

  const describeMutationFailure = useCallback(async (
    name: string,
    action: string,
    error: unknown,
  ): Promise<string> => {
    try {
      await refreshJobs();
      return `${action}: ${error} · refreshed the job registry; no unseen changes were overwritten`;
    } catch (refreshError) {
      return `${action}: ${error} · job-registry refresh failed: ${refreshError}`;
    }
  }, [refreshJobs]);

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
    setAdvancedFiltersAnchor(null);
    setContextMenu(null);
    setAskSwap(null);
    setCandidateAdoption(null);
  }, [resetCompareReview, resetConfirmation]);

  const clearAdvancedFilters = useCallback(() => {
    workspaceController.applyAdvancedFilter({ ...EMPTY_ADVANCED_SCOPE_FILTER, masks: [] });
    setAdvancedFiltersAnchor(null);
  }, [workspaceController]);

  const clearRunScope = useCallback(() => {
    if (selectedCompareWorkspace) {
      workspaceController.changeDifferenceSearchDraft('');
      dispatchCompareWorkspace({
        type: 'selected_result_types_changed',
        resultKey: selectedCompareWorkspace.key,
        resultTypes: EMPTY_RESULT_TYPES,
      });
      dispatchCompareWorkspace({ type: 'folder_scope_changed', resultKey: selectedCompareWorkspace.key, folderScope: null });
      workspaceController.applyAdvancedFilter({ ...EMPTY_ADVANCED_SCOPE_FILTER, masks: [] });
    }
    setAdvancedFiltersAnchor(null);
  }, [selectedCompareWorkspace, workspaceController]);

  const acceptAutoScanStatus = useCallback((
    incoming: AutoScanStatusDto,
    source: AutoScanStatusSource,
    declinedTicket?: AutoScanTicket,
  ) => {
    const current = autoScanStatusRef.current;
    const next = reconcileAutoScanStatus(current, incoming, source, declinedTicket);
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

  const previousReviewKey = useRef<string | null>(reviewKey);
  useEffect(() => {
    if (previousReviewKey.current === reviewKey) return;
    previousReviewKey.current = reviewKey;
    if (!confirmOpen) return;
    resetConfirmation();
    setStatus('The reviewed action set changed — open confirmation again', 'err');
  }, [reviewKey, confirmOpen, resetConfirmation, setStatus]);

  useEffect(() => {
    if (!reviewEditable) setContextMenu(null);
  }, [reviewEditable]);

  const previousCompareReviewKey = useRef<string | null>(currentCompareReviewKey);
  useEffect(() => {
    if (previousCompareReviewKey.current === currentCompareReviewKey) return;
    previousCompareReviewKey.current = currentCompareReviewKey;
    resetCompareReview();
  }, [currentCompareReviewKey, resetCompareReview]);

  useEffect(() => {
    if (!selectedJob || selectedJob.targets.length === 0 || selectedTargetIndex < selectedJob.targets.length) return;
    setSelectedTargetIndex(0);
    resetSafetyUi();
    setStatus(`'${selectedJob.name}' no longer has target ${selectedTargetIndex + 1}; selected target 1`);
  }, [selectedJob, selectedTargetIndex, resetSafetyUi, setStatus]);

  const expireDeletedJobState = useCallback((jobId: string) => {
    dispatchCompareWorkspace({ type: 'job_execution_expired', jobId, reason: 'job_deleted' });
    dispatchRootEditor({ type: 'job_removed', jobId });
  }, []);

  const replaceIncludedRows = useCallback((next: boolean[] | ((prev: boolean[]) => boolean[])) => {
    if (!selectedCompareWorkspace || !reviewEditable) return;
    dispatchCompareWorkspace({
      type: 'row_inclusion_replaced',
      resultKey: selectedCompareWorkspace.key,
      rowIncluded: typeof next === 'function' ? next(selectedCompareWorkspace.differences.rowIncluded) : next,
    });
  }, [selectedCompareWorkspace, reviewEditable]);

  const replaceReversedRows = useCallback((next: boolean[] | ((prev: boolean[]) => boolean[])) => {
    if (!selectedCompareWorkspace || !reviewEditable) return;
    dispatchCompareWorkspace({
      type: 'row_reversal_replaced',
      resultKey: selectedCompareWorkspace.key,
      rowReversed: typeof next === 'function' ? next(selectedCompareWorkspace.differences.rowReversed) : next,
    });
  }, [selectedCompareWorkspace, reviewEditable]);

  const beginCompareActivity = useCallback((
    comparedJob: JobIdentitySnapshot,
    targetIndex: number,
    origin: { kind: 'interactive' } | { kind: 'auto_scan'; generation: number; ticketId: number },
  ): CompareActivityRequest => {
    const requestId = compareActivityRequestId.current + 1;
    compareActivityRequestId.current = requestId;
    const scope = {
      job_id: comparedJob.jobId,
      target_index: targetIndex,
      config_revision: comparedJob.configRevision,
    };
    dispatchCompareWorkspace({ type: 'compare_activity_started', scope, requestId, origin });
    return { scope, requestId };
  }, []);

  const failCompareActivity = useCallback((activity: CompareActivityRequest, error: string) => {
    dispatchCompareWorkspace({
      type: 'compare_activity_failed',
      scopeKey: compareScopeKey(activity.scope),
      requestId: activity.requestId,
      error,
    });
  }, []);

  const requestResultRestore = useCallback((
    job: JobDto,
    targetIndex: number,
    retained: ReturnType<typeof activeWorkspace>,
    announce = true,
  ) => {
    const requestId = restoreRequestId.current + 1;
    restoreRequestId.current = requestId;
    const scope = compareScopeForJob(job, targetIndex);
    const scopeKey = compareScopeKey(scope);
    const selectionStillOwnsScope = () => {
      const selected = selectionRef.current;
      return !!selected.job
        && compareScopeKey(compareScopeForJob(selected.job, selected.targetIndex)) === scopeKey;
    };
    if (!retained) {
      dispatchCompareWorkspace({ type: 'scope_restore_started', scope, requestId });
      void ipc.restoreCompare(job.job_id, targetIndex, job.config_revision).then((lookup) => {
        dispatchCompareWorkspace({
          type: 'scope_restore_completed',
          scopeKey,
          requestId,
          lookup,
          preferences: workspacePreferences,
        });
        const lookupProblem = scopeWorkspaceLookupProblem(scope, lookup);
        if (announce && selectionStillOwnsScope() && lookupProblem) {
          setStatus(`Could not restore '${job.name}': ${lookupProblem}`, 'err');
        } else if (announce && selectionStillOwnsScope()) {
          setStatus(lookup.status === 'found'
            ? `${job.name} · restored ${lookup.workspace.plan.ops.length} compare items`
            : `${job.name} · no retained result — Compare to create one`);
        }
      }).catch(async (error) => {
        dispatchCompareWorkspace({ type: 'scope_restore_failed', scopeKey, requestId, error: String(error) });
        const requestOwnedSelection = selectionStillOwnsScope();
        let refreshFailure: unknown = null;
        try {
          const list = await refreshJobs();
          const refreshed = list.find((candidate) => candidate.job_id === job.job_id) ?? null;
          reconcileWorkspaceJob(snapshotJobIdentity(job), refreshed);
        } catch (refreshError) {
          refreshFailure = refreshError;
        }
        if (announce && requestOwnedSelection) {
          setStatus(
            `Could not restore '${job.name}' result: ${error}`
              + (refreshFailure ? ` · job-list refresh failed: ${refreshFailure}` : ' · refreshed the job registry'),
            'err',
          );
        }
      });
      return;
    }
    dispatchCompareWorkspace({ type: 'scope_touched', scope });
    dispatchCompareWorkspace({ type: 'workspace_lookup_started', workspace: retained, requestId });
    void ipc.reconcileCompareWorkspace(retained.identity).then((lookup) => {
      dispatchCompareWorkspace({
        type: 'workspace_lookup_completed',
        resultKey: retained.key,
        requestId,
        lookup,
      });
      if (announce && selectionStillOwnsScope()) {
        const lookupProblem = exactWorkspaceLookupProblem(scope, retained.key, lookup);
        if (lookupProblem) {
          setStatus(`Could not confirm '${job.name}': ${lookupProblem}`, 'err');
        } else if (lookup.status === 'missing') {
          setStatus(`${job.name} · retained result missing — Compare again`, 'err');
        }
      }
    }).catch((error) => {
      dispatchCompareWorkspace({
        type: 'workspace_lookup_failed',
        resultKey: retained.key,
        requestId,
        error: String(error),
      });
      if (announce && selectionStillOwnsScope()) {
        setStatus(`Could not confirm '${job.name}' result: ${error}`, 'err');
      }
    });
  }, [reconcileWorkspaceJob, refreshJobs, setStatus, workspacePreferences]);

  const runAuthorizedCompare = useCallback(async (
    authorizationToken: string,
    comparedJob: JobIdentitySnapshot,
    targetIndex: number,
    autoTicket?: AutoScanTicket,
    prestartedActivity?: CompareActivityRequest,
  ): Promise<CompareCompletion | null> => {
    const interaction = liveInteractionState.current;
    if (interaction.busy
      || interaction.editorOpen
      || interaction.settingsOpen
      || interaction.confirmationOpen
      || interaction.candidateAdoptionOpen
      || interaction.rootDraftOpen
      || interaction.rootSwapOpen
      || interaction.contextMenuOpen
      || compareInFlight.current
      || autoApplyInFlight.current) {
      if (prestartedActivity) failCompareActivity(prestartedActivity, 'Another interaction took ownership before Compare launched');
      return null;
    }
    if (autoTicket && (
      !monitorOwnsAutoScanTicket(autoScanStatusRef.current, autoScanTicket.current, autoTicket)
      || applyExecutionRequest.current !== null
      || applyReviewRequest.current !== null
      || compareReviewRequest.current !== null
      || interaction.confirmationOpen
      || interaction.reviewPending
    )) {
      if (prestartedActivity) failCompareActivity(prestartedActivity, 'The AutoScan ticket lost execution ownership before Compare launched');
      return null;
    }
    if (!autoTicket) autoScanTicket.current = null;
    const showProgress = !autoTicket;
    const activity = prestartedActivity ?? beginCompareActivity(
      comparedJob,
      targetIndex,
      autoTicket
        ? { kind: 'auto_scan', generation: autoTicket.generation, ticketId: autoTicket.ticketId }
        : { kind: 'interactive' },
    );
    compareInFlight.current = true;
    const name = comparedJob.name;
    if (!autoTicket) resetSafetyUi();
    if (!autoTicket) setBusy(true);
    setStatus(`${autoTicket ? 'AutoScan is comparing' : 'Comparing'} '${name}'…`);
    if (showProgress) setCompareStages([]);
    compareRateByPhase.current.clear();
    if (showProgress) setCompareCancelling(false);
    compareRunFloor.current = compareRunId.current;
    compareRunReady.current = false;
    if (showProgress) setCompareActive(true);
    try {
      const snapshot = await ipc.compareJob(authorizationToken);
      const comparedPlan = snapshot.plan;
      dispatchCompareWorkspace(autoTicket
        ? {
          type: 'autoscan_compare_published',
          snapshot,
          generation: autoTicket.generation,
          ticketId: autoTicket.ticketId,
          preferences: workspacePreferences,
        }
        : { type: 'manual_compare_published', snapshot, preferences: workspacePreferences });
      // A job file can be edited outside the app while it is open. Compare used the authoritative
      // file, so refresh the list row before deciding whether the returned owner belongs on screen.
      // Snapshot first: refreshJobs may commit the new row (or null) before this continuation runs,
      // and then the ref no longer tells us that the selected job changed underneath this compare.
      const selectedBeforeRefresh = selectionRef.current;
      let refreshedJob: JobDto | null = null;
      let refreshProblem: unknown = null;
      try {
        const list = await refreshJobs();
        refreshedJob = list.find((job) => job.job_id === comparedPlan.owner.identity.job_id) ?? null;
        reconcileWorkspaceJob({
          jobId: comparedPlan.owner.identity.job_id,
          name: comparedPlan.owner.job_name,
          configRevision: comparedPlan.owner.identity.config_revision,
        }, refreshedJob);
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
        } else if (selectedBeforeRefresh.job.config_revision !== refreshedJob.config_revision) {
          resetSafetyUi();
        }
      }
      const resultBelongsToSelection = !!selectedJob
        && comparedPlan.owner.identity.job_id === selectedJob.job_id
        && comparedPlan.owner.identity.config_revision === selectedJob.config_revision
        && comparedPlan.owner.identity.target_index === selected.targetIndex;
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
      dispatchCompareWorkspace({
        type: 'compare_activity_finished',
        scopeKey: compareScopeKey(activity.scope),
        requestId: activity.requestId,
      });
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
        const list = await refreshJobs();
        const refreshedJob = list.find((job) => job.job_id === comparedJob.jobId) ?? null;
        reconcileWorkspaceJob(comparedJob, refreshedJob);
        if (selected.job?.job_id === comparedJob.jobId) {
          if (!refreshedJob) {
            setSelectedTargetIndex(0);
            suffix = ` · '${name}' is no longer a registered job`;
          } else if (selected.job.config_revision !== refreshedJob.config_revision) {
            resetSafetyUi();
            suffix = ' · refreshed the changed job configuration';
          }
        }
      } catch (refreshError) {
        refreshProblem = refreshError;
      }
      const base = cancelled ? 'Compare cancelled' : `${autoTicket ? 'AutoScan Compare' : 'Compare'} failed: ${error}`;
      if (refreshProblem) suffix = ` · job-list refresh failed: ${refreshProblem}`;
      failCompareActivity(activity, String(error));
      setStatus(`${base}${suffix}`, cancelled && !refreshProblem ? '' : 'err');
      return null;
    } finally {
      if (showProgress) setCompareActive(false);
      if (!autoTicket) setBusy(false);
      compareRunReady.current = false;
      compareInFlight.current = false;
    }
  }, [beginCompareActivity, failCompareActivity, refreshJobs, reconcileWorkspaceJob, resetSafetyUi, setStatus, workspacePreferences]);

  const doCompare = useCallback(async (autoTicket?: AutoScanTicket): Promise<CompareCompletion | null> => {
    if (busy || editor || compareInFlight.current || applyExecutionRequest.current || autoApplyInFlight.current) return null;
    if (!autoTicket && !selectedJob) return null;
    if (!autoTicket && autoScanTicket.current !== null) {
      setStatus('Compare is unavailable while AutoScan owns a verification ticket', 'err');
      return null;
    }
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
      : snapshotJobIdentity(selectedJob!);
    const targetIndex = autoTicket?.targetIndex ?? selectedTargetIndex;
    const key = compareReviewKey(comparedJob.jobId, comparedJob.configRevision, targetIndex);

    if (autoTicket) {
      const activity = beginCompareActivity(comparedJob, targetIndex, {
        kind: 'auto_scan',
        generation: autoTicket.generation,
        ticketId: autoTicket.ticketId,
      });
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
        if (!stillOwned) {
          failCompareActivity(activity, 'The AutoScan ticket was superseded during authorization review');
          return null;
        }
        const authorization = directAuthorization(review);
        if (!authorization) {
          setStatus(
            review.status === 'direct_authorized'
              ? `AutoScan paused: the direct Compare review for '${comparedJob.name}' was internally inconsistent`
              : `AutoScan paused: Compare requires an exact interactive authorization for '${comparedJob.name}'`,
            'err',
          );
          failCompareActivity(activity, 'Interactive Compare authorization is required');
          return null;
        }
        return runAuthorizedCompare(
          authorization.authorization_token,
          comparedJob,
          targetIndex,
          autoTicket,
          activity,
        );
      } catch (error) {
        failCompareActivity(activity, String(error));
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
  }, [selectedJob, busy, editor, compareReview, applyReview, confirmOpen, selectedTargetIndex, beginCompareActivity, failCompareActivity, runAuthorizedCompare, setStatus]);

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
        snapshotJobIdentity(selected.job),
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
    if (!selectedJob
      || !plan
      || !reviewKey
      || busy
      || compareInFlight.current
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
      const review = await ipc.reviewApply(plan.owner.identity, reviewedRowDecisions);
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
  }, [applyAvailability, selectedJob, plan, reviewKey, busy, applyReview, compareReview.review, reviewedRowDecisions, setStatus]);

  const doSync = useCallback(async () => {
    if (
      !selectedJob
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
    const executionDecisions = reviewedRowDecisions;
    const applyingJob = selectedJob;
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
      setStatus(`Synchronizing '${applyingJob.name}' (${executionDecisions.length} items)...`);
      // The command returns only after the new window has installed its run-progress listener.
      // Starting apply any earlier loses the phase start/totals on a freshly opened window.
      launchId = await ipc.openProgressWindow();
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
      refreshLatestRunSummaries();
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
      requestResultRestore(applyingJob, selectedTargetIndex, selectedCompareWorkspace, false);
    } finally {
      if (launchId !== null) {
        void ipc.cancelProgressLaunch(launchId).catch((error) => {
          setStatus(`Could not release progress launch ${launchId}: ${error}`, 'err');
        });
      }
      if (applyExecutionRequest.current?.requestId === request.requestId
        && applyExecutionRequest.current.key === request.key) {
        applyExecutionRequest.current = null;
      }
    }
  }, [selectedJob, selectedCompareWorkspace, plan, reviewKey, confirmOpen, confirmReviewKey, applyAvailability, applyReview, applyChoices, busy, reviewedRowDecisions, selectedTargetIndex, doCompare, refreshLatestRunSummaries, requestResultRestore, resetConfirmation, resetSafetyUi, setStatus]);

  const selectJob = useCallback((job: JobDto) => {
    if (selectedJob?.job_id === job.job_id) return;
    const targetIndex = preferredTargetIndex(compareWorkspaceRepository, job);
    const restored = activeWorkspace(compareWorkspaceRepository, job, targetIndex);
    selectionRef.current = { job, targetIndex };
    setRegistrySelection(job);
    setSelectedTargetIndex(targetIndex);
    resetSafetyUi();
    setStatus(restored
      ? `${job.name} · restored ${restored.plan.ops.length} compare items`
      : `${job.name} · ${job.mode}${job.rigor !== 'standard' ? ` · ${job.rigor}` : ''}`);
    requestResultRestore(job, targetIndex, restored);
  }, [selectedJob?.job_id, compareWorkspaceRepository, requestResultRestore, resetSafetyUi, setRegistrySelection, setStatus]);

  const saveRootDraft = useCallback(async (field: RootField) => {
    const workspace = liveRootEditor.current;
    const interaction = liveInteractionState.current;
    if (!workspace
      || !rootDraftIsDirty(workspace, field)
      || workspace.save.status === 'saving'
      || rootSaveInFlight.current !== null) return;
    if (workspace.conflicts[field]) {
      setStatus(`The saved ${field} changed — choose Keep draft or Cancel before saving`, 'err');
      return;
    }
    if (interaction.busy
      || interaction.editorOpen
      || interaction.settingsOpen
      || interaction.confirmationOpen
      || interaction.candidateAdoptionOpen
      || interaction.rootSwapOpen
      || interaction.contextMenuOpen
      || interaction.reviewPending
      || compareInFlight.current
      || autoApplyInFlight.current
      || (autoScanStatusRef.current?.active_ticket ?? null) !== null
      || autoScanTicket.current !== null) {
      setStatus(`Cannot save ${field} while another review or execution owns the job`, 'err');
      return;
    }
    const value = workspace.draft[field].trim();
    if (!value) {
      setStatus(`${field === 'source' ? 'Source' : 'Target'} cannot be empty`, 'err');
      return;
    }
    const requestId = rootSaveRequestId.current + 1;
    rootSaveRequestId.current = requestId;
    const workspaceKey = workspace.key;
    const owner = workspace.owner;
    const before = workspace.committed[field];
    rootSaveInFlight.current = { workspaceKey, requestId };
    dispatchRootEditor({ type: 'save_started', workspaceKey, requestId, field });
    let result: ipc.JobRootMutationDto;
    try {
      result = await ipc.updateJobRoot(
        owner.jobName,
        owner.jobId,
        owner.configRevision,
        owner.targetIndex,
        field,
        value,
      );
    } catch (error) {
      dispatchRootEditor({ type: 'save_failed', workspaceKey, requestId, error: String(error) });
      setStatus(await describeMutationFailure(
        owner.jobName,
        `Could not save ${field}; the draft was retained`,
        error,
      ), 'err');
      return;
    } finally {
      if (rootSaveInFlight.current?.workspaceKey === workspaceKey
        && rootSaveInFlight.current.requestId === requestId) {
        rootSaveInFlight.current = null;
      }
    }
    const committed = rootMutationState(result, owner.targetIndex);
    dispatchRootEditor({
      type: 'save_committed',
      workspaceKey,
      requestId,
      owner: committed.owner,
      values: committed.values,
    });
    reconcileSavedWorkspaceJob(result.mutation, {
      jobId: owner.jobId,
      name: owner.jobName,
      configRevision: owner.configRevision,
    });
    resetSafetyUi();
    pushHistory(value);
    const undo = async () => {
      let restored: ipc.JobRootMutationDto;
      try {
        restored = await ipc.updateJobRoot(
          result.mutation.name,
          result.mutation.job_id,
          result.mutation.config_revision,
          owner.targetIndex,
          field,
          before,
        );
      } catch (error) {
        throw new Error(await describeMutationFailure(
          result.mutation.name,
          `Could not restore ${field}`,
          error,
        ));
      }
      const restoredState = rootMutationState(restored, owner.targetIndex);
      dispatchRootEditor({
        type: 'workspace_rebound',
        workspaceKey,
        owner: restoredState.owner,
        values: restoredState.values,
      });
      reconcileSavedWorkspaceJob(restored.mutation, {
        jobId: result.mutation.job_id,
        name: result.mutation.name,
        configRevision: result.mutation.config_revision,
      });
      resetSafetyUi();
      const warning = statusDeliveryWarning(restored.mutation);
      try {
        await refreshJobs();
        setStatus(`Restored ${field}${warning}`, warning ? 'err' : '');
      } catch (error) {
        setStatus(`Restored ${field}${warning} · job-list refresh failed: ${error}`, 'err');
      }
    };
    const warning = statusDeliveryWarning(result.mutation);
    const success = `Changed ${field} → ${value} — Compare again (Ctrl+R)${warning}`;
    try {
      await refreshJobs();
      setStatusAction(success, `Undo ${field} change`, undo, warning ? 'err' : '');
    } catch (error) {
      setStatusAction(`${success} · job-list refresh failed: ${error}`, `Undo ${field} change`, undo, 'err');
    }
  }, [describeMutationFailure, pushHistory, reconcileSavedWorkspaceJob, refreshJobs, resetSafetyUi, setStatus, setStatusAction]);

  const requestSwap = useCallback(() => {
    const workspace = liveRootEditor.current;
    const selectedJob = selectionRef.current.job;
    if (!workspace
      || !selectedJob
      || busy
      || compareInFlight.current
      || autoApplyInFlight.current
      || (autoScanStatusRef.current?.active_ticket ?? null) !== null
      || autoScanTicket.current !== null) return;
    if (rootDraftIsDirty(workspace, 'source') || rootDraftIsDirty(workspace, 'target')) {
      setStatus('Save or cancel the root drafts before swapping', 'err');
      return;
    }
    if (workspace.owner.jobId !== selectedJob.job_id
      || workspace.owner.configRevision !== selectedJob.config_revision
      || workspace.owner.targetIndex !== selectionRef.current.targetIndex) {
      setStatus('The selected root editor is stale; wait for the job registry to finish refreshing', 'err');
      return;
    }
    setAskSwap({
      workspaceKey: workspace.key,
      owner: workspace.owner,
      values: workspace.committed,
      mode: selectedJob.mode,
    });
  }, [busy, setStatus]);

  const doSwap = useCallback(async (request: RootSwapRequest) => {
    setBusy(true);
    let result: ipc.JobRootMutationDto;
    try {
      result = await ipc.swapJobRoots(
        request.owner.jobName,
        request.owner.jobId,
        request.owner.configRevision,
        request.owner.targetIndex,
      );
    } catch (error) {
      setBusy(false);
      setStatus(await describeMutationFailure(
        request.owner.jobName,
        'Root swap failed',
        error,
      ), 'err');
      return;
    }
    const committed = rootMutationState(result, request.owner.targetIndex);
    dispatchRootEditor({
      type: 'workspace_rebound',
      workspaceKey: request.workspaceKey,
      owner: committed.owner,
      values: committed.values,
    });
    reconcileSavedWorkspaceJob(result.mutation, {
      jobId: request.owner.jobId,
      name: request.owner.jobName,
      configRevision: request.owner.configRevision,
    });
    resetSafetyUi();
    pushHistory(committed.values.source);
    pushHistory(committed.values.target);
    setBusy(false);
    const undo = async () => {
      let restored: ipc.JobRootMutationDto;
      try {
        restored = await ipc.swapJobRoots(
          result.mutation.name,
          result.mutation.job_id,
          result.mutation.config_revision,
          request.owner.targetIndex,
        );
      } catch (error) {
        throw new Error(await describeMutationFailure(
          result.mutation.name,
          'Could not undo the root swap',
          error,
        ));
      }
      const restoredState = rootMutationState(restored, request.owner.targetIndex);
      dispatchRootEditor({
        type: 'workspace_rebound',
        workspaceKey: request.workspaceKey,
        owner: restoredState.owner,
        values: restoredState.values,
      });
      reconcileSavedWorkspaceJob(restored.mutation, {
        jobId: result.mutation.job_id,
        name: result.mutation.name,
        configRevision: result.mutation.config_revision,
      });
      resetSafetyUi();
      const warning = statusDeliveryWarning(restored.mutation);
      try {
        await refreshJobs();
        setStatus(`Restored the two roots of '${restored.mutation.name}'${warning}`, warning ? 'err' : '');
      } catch (error) {
        setStatus(`Restored the two roots of '${restored.mutation.name}'${warning} · job-list refresh failed: ${error}`, 'err');
      }
    };
    const warning = statusDeliveryWarning(result.mutation);
    const success = `Swapped target ${request.owner.targetIndex + 1} and source for '${result.mutation.name}' — Compare again (Ctrl+R)${warning}`;
    try {
      await refreshJobs();
      setStatusAction(success, 'Undo swap', undo, warning ? 'err' : '');
    } catch (error) {
      setStatusAction(`${success} · job-list refresh failed: ${error}`, 'Undo swap', undo, 'err');
    }
  }, [describeMutationFailure, pushHistory, reconcileSavedWorkspaceJob, refreshJobs, resetSafetyUi, setStatus, setStatusAction]);

  /// Write an exclude back into the job's exclude list. Pruning during the scan only takes effect at the
  /// next Compare, so the message has to say so and leave an undo behind.
  const addExcludes = useCallback(async (masks: string[], label: string) => {
    if (!selectedJob) { setStatus('Select a job first', 'err'); return; }
    const name = selectedJob.name;
    let detail: ipc.JobDetailDto;
    try {
      detail = await ipc.getJob(name);
    } catch (error) {
      setStatus(await describeMutationFailure(
        name,
        'Failed to read the job before adding the exclude',
        error,
      ), 'err');
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
      setStatus(await describeMutationFailure(name, 'Failed to write exclude', error), 'err');
      return;
    }
    reconcileSavedWorkspaceJob(
      saved,
      { jobId: detail.job_id, name: detail.name, configRevision: detail.config_revision },
    );
    resetSafetyUi();
    const undo = async () => {
      let restored: ipc.JobSaveDto;
      try {
        restored = await ipc.saveJob(saved.name, jobConfiguration, {
          originalName: saved.name,
          expectedRevision: saved.config_revision,
        });
        reconcileSavedWorkspaceJob(
          restored,
          { jobId: saved.job_id, name: saved.name, configRevision: saved.config_revision },
        );
      } catch (error) {
        throw new Error(await describeMutationFailure(
          saved.name,
          'Could not undo the exclude',
          error,
        ));
      }
      resetSafetyUi();
      const warning = statusDeliveryWarning(restored);
      try {
        await refreshJobs();
        setStatus(`Exclude undone${warning}`, warning ? 'err' : '');
      } catch (error) {
        setStatus(`Exclude undone${warning} · job-list refresh failed: ${error}`, 'err');
      }
    };
    const warning = statusDeliveryWarning(saved);
    const success = `${label}: ${addedMasks.join(', ')} — Compare again to build a result with this exclusion${warning}`;
    try {
      await refreshJobs();
      setStatusAction(success, 'Undo exclude', undo, warning ? 'err' : '');
    } catch (error) {
      setStatusAction(`${success} · job-list refresh failed: ${error}`, 'Undo exclude', undo, 'err');
    }
  }, [describeMutationFailure, selectedJob, reconcileSavedWorkspaceJob, refreshJobs, resetSafetyUi, setStatus, setStatusAction]);

  const exportCsv = useCallback(async () => {
    if (!plan || !selectedCompareWorkspace) { setStatus('Compare first', 'err'); return; }
    if (resultView !== 'differences') { setStatus('Switch to Differences before exporting', 'err'); return; }
    if (scopeCalculationFailed) { setStatus('The run scope could not be calculated safely', 'err'); return; }
    if (scopeCalculationPending) { setStatus('The run scope is still being calculated', 'err'); return; }
    if (csvExportInFlight.current !== null) return;
    const resultKey = selectedCompareWorkspace.key;
    const compareIdentity = selectedCompareWorkspace.identity;
    const rowPresentation = layout.displayOrder.map((index) => ({
      index,
      included: includedRows[index] === true,
      direction_reversed: reversedRows[index] === true,
    }));
    const scopeLabel = selectedJob
      ? `'${selectedJob.name}', target ${compareIdentity.target_index + 1}`
      : `job ${compareIdentity.job_id}, target ${compareIdentity.target_index + 1}`;
    csvExportInFlight.current = resultKey;
    setCsvExportPending(true);
    try {
      const result = await ipc.exportCompareCsv(
        compareIdentity,
        rowPresentation,
      );
      if (result.status === 'cancelled') return;
      const scopeSuffix = selectedCompareWorkspaceKeyRef.current === resultKey ? '' : ` from ${scopeLabel}`;
      setStatusAction(
        `Exported ${result.row_count} rows${scopeSuffix} to ${result.display_path}`,
        'Open containing folder',
        () => ipc.revealCsvExport(result.receipt_id),
      );
    } catch (error) {
      setStatus(`Export failed for ${scopeLabel}: ${error}`, 'err');
    } finally {
      if (csvExportInFlight.current === resultKey) {
        csvExportInFlight.current = null;
        setCsvExportPending(false);
      }
    }
  }, [plan, selectedCompareWorkspace, selectedJob, resultView, scopeCalculationFailed, scopeCalculationPending, layout, reversedRows, includedRows, setStatus, setStatusAction]);

  const changeRootDraft = useCallback((field: RootField, value: string) => {
    const workspace = liveRootEditor.current;
    if (!workspace) return;
    dispatchRootEditor({ type: 'draft_changed', workspaceKey: workspace.key, field, value });
  }, []);

  const revertRootDraft = useCallback((field: RootField) => {
    const workspace = liveRootEditor.current;
    if (!workspace) return;
    dispatchRootEditor({ type: 'draft_reverted', workspaceKey: workspace.key, field });
  }, []);

  const acceptRootDraftConflict = useCallback((field: RootField) => {
    const workspace = liveRootEditor.current;
    if (!workspace) return;
    dispatchRootEditor({ type: 'draft_conflict_accepted', workspaceKey: workspace.key, field });
  }, []);

  const browseRoot = useCallback(async (field: RootField) => {
    const workspace = liveRootEditor.current;
    if (!workspace) return;
    const requestId = rootPickerRequestId.current + 1;
    rootPickerRequestId.current = requestId;
    try {
      const selectedPath = await ipc.pickDirectory({
        title: `Select the ${field} directory`,
        defaultPath: workspace.draft[field].trim() || workspace.committed[field],
      });
      if (!selectedPath) return;
      const currentWorkspace = liveRootEditor.current;
      if (rootPickerRequestId.current !== requestId || currentWorkspace?.key !== workspace.key) {
        setStatus('The directory selection was ignored because the selected job changed while the picker was open');
        return;
      }
      dispatchRootEditor({
        type: 'draft_changed',
        workspaceKey: workspace.key,
        field,
        value: selectedPath,
      });
      pushHistory(selectedPath);
      setStatus(`Selected ${field} draft → ${selectedPath}. Choose Save to update the job.`);
    } catch (error) {
      if (rootPickerRequestId.current !== requestId) return;
      setStatus(`Can't open the picker: ${error}`, 'err');
    }
  }, [pushHistory, setStatus]);

  const setRowIncluded = useCallback((index: number, value: boolean) => {
    replaceIncludedRows((previous) => { const next = [...previous]; next[index] = value; return next; });
  }, [replaceIncludedRows]);

  const setRowsIncluded = useCallback((indices: number[], value: boolean) => {
    replaceIncludedRows((previous) => {
      const next = [...previous];
      for (const index of indices) next[index] = value;
      return next;
    });
  }, [replaceIncludedRows]);

  const toggleRowDirection = useCallback((index: number) => {
    replaceReversedRows((previous) => {
      const next = [...previous];
      next[index] = !next[index];
      return next;
    });
  }, [replaceReversedRows]);

  const toggleFolderFold = useCallback((folderPath: string) => {
    if (!selectedCompareWorkspace) return;
    dispatchCompareWorkspace({
      type: 'difference_folder_fold_toggled',
      resultKey: selectedCompareWorkspace.key,
      folderPath,
    });
  }, [selectedCompareWorkspace]);

  /// Click a header to sort: clicking the same key again flips the direction, a third click clears back
  /// to the plan's order
  const toggleSort = useCallback((key: SortKey) => {
    if (!selectedCompareWorkspace) return;
    const { natural } = keySpec(key);
    const nextSort: Sort | null = !sort || sort.key !== key
      ? { key, dir: natural }
      : sort.dir === natural
        ? { key, dir: sort.dir === 1 ? -1 : 1 }
        : null;
    dispatchCompareWorkspace({
      type: 'difference_sort_changed',
      resultKey: selectedCompareWorkspace.key,
      sort: nextSort,
    });
  }, [selectedCompareWorkspace, sort]);

  const rowMenu = useCallback((index: number, x: number, y: number) => {
    if (!plan) return;
    const operation = effectiveOperation(plan, reversedRows, index);
    const [sourcePath, targetPath] = sidePaths(operation);
    const sourceAbsolutePath = sourcePath ? joinDisplayPath(plan.header.source_root, sourcePath) : null;
    const targetAbsolutePath = targetPath ? joinDisplayPath(plan.header.target_root, targetPath) : null;
    const relativePath = operation.path;
    const baseName = relativePathBaseName(relativePath);
    const extensionSeparator = baseName.lastIndexOf('.');
    const extension = extensionSeparator > 0 ? baseName.slice(extensionSeparator + 1) : '';
    const folderPath = owningFolderOf(operation);
    const inFolderScope = inScopeIndices.filter((candidateIndex) => (
      matchesFolderScope(effectiveOperation(plan, reversedRows, candidateIndex), folderPath)
      && isExecutableOperation(effectiveOperation(plan, reversedRows, candidateIndex))
    ));
    const copyPath = (path: string) => navigator.clipboard?.writeText(path).then(
      () => setStatus(`Copied: ${path}`),
      () => setStatus('Copy failed (clipboard unavailable)', 'err'),
    );
    setContextMenu({
      x, y,
      entries: [
        {
          label: 'Show in File Manager · Source',
          disabled: !sourceAbsolutePath,
          run: () => {
            ipc.revealCompareRow(plan.owner.identity, index, 'source', reversedRows[index] === true)
              .catch((error) => setStatus(`Could not reveal the source item: ${error}`, 'err'));
          },
        },
        {
          label: 'Show in File Manager · Target',
          disabled: !targetAbsolutePath,
          run: () => {
            ipc.revealCompareRow(plan.owner.identity, index, 'target', reversedRows[index] === true)
              .catch((error) => setStatus(`Could not reveal the target item: ${error}`, 'err'));
          },
        },
        { separator: true, label: '' },
        { label: 'Copy Full Path', run: () => copyPath((sourceAbsolutePath ?? targetAbsolutePath)!) },
        { label: 'Copy Relative Path', run: () => copyPath(relativePath) },
        { separator: true, label: '' },
        { label: extension ? `Exclude This Type */*.${extension}` : 'Exclude This Type (No Extension)', disabled: !extension || !selectedJob, run: () => addExcludes([`*/*.${extension}`], 'Added to exclude') },
        { label: folderPath ? `Exclude This Directory /${folderPath}/` : 'Exclude This Directory (Already at the Root)', disabled: !folderPath || !selectedJob, run: () => addExcludes([`/${folderPath}/`], 'Added to exclude') },
        { separator: true, label: '' },
        {
          label: reversedRows[index] ? 'Restore Original Direction' : 'Reverse This Row',
          disabled: !reviewEditable || !canReverseOperation(plan, index),
          run: () => toggleRowDirection(index),
        },
        {
          label: 'Include Only This Item',
          disabled: !reviewEditable,
          run: () => replaceIncludedRows(plan.ops.map((_, candidateIndex) => (
            candidateIndex === index
            && isExecutableOperation(effectiveOperation(plan, reversedRows, candidateIndex))
          ))),
        },
        {
          label: `${folderPath ? 'Exclude This Folder and Subfolders' : 'Exclude Root-Level Items'} (${inFolderScope.length})`,
          disabled: !reviewEditable || inFolderScope.length === 0,
          run: () => setRowsIncluded(inFolderScope, false),
        },
      ],
    });
  }, [plan, reversedRows, inScopeIndices, selectedJob, reviewEditable, addExcludes, toggleRowDirection, setRowsIncluded, replaceIncludedRows, setStatus]);

  useEffect(() => {
    (async () => {
      try {
        const list = await refreshJobs();
        refreshLatestRunSummaries();
        setJobsDir(await ipc.jobsDir());
        let versionError: unknown = null;
        try {
          setAppVersion('v' + (await getVersion()));
        } catch (error) {
          versionError = error;
          setAppVersion('version unavailable');
        }
        if (versionError) {
          setStatus(`Initialized, but the application version could not be read: ${versionError}`, 'err');
        } else {
          setStatus(list.length ? 'Select a job on the left to start' : 'No jobs — drop a <name>.toml into the jobs directory');
        }
      } catch (error) {
        setStatus(`Init failed: ${error}`, 'err');
      }
    })();
  }, [refreshJobs, refreshLatestRunSummaries, setStatus]);

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
      // Error events are actionable even when the producer cannot associate them with a phase.
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
          replay = await ipc.replayCompareEvents();
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
    let disposed = false;
    let dispose: (() => void) | undefined;
    void listen<string>('main-close-blocked', (event) => {
      setStatus(event.payload, 'err');
    }).then((unlisten) => {
      if (disposed) unlisten();
      else dispose = unlisten;
    }).catch((error) => {
      if (!disposed) setStatus(`Could not subscribe to close-blocked status: ${error}`, 'err');
    });
    return () => {
      disposed = true;
      dispose?.();
    };
  }, [setStatus]);

  // Tauri reports desktop file drops through Webview.onDragDropEvent in physical coordinates:
  // https://v2.tauri.app/reference/javascript/api/namespacewebview/#ondragdropevent
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
      const input = [...(scope?.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>('[data-drop]') ?? [])]
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
        } catch (error) {
          setStatus(`Could not inspect the dropped path, so it was kept as-is: ${error}`, 'err');
        }
        pushHistory(pathValue);
        if (input.dataset.root === 'source' || input.dataset.root === 'target') {
          changeRootDraft(input.dataset.root as RootField, pathValue);
          setStatus(`Filled the ${input.dataset.root} draft → ${pathValue}. Choose Save to update the job.`);
        } else {
          editorApi.current?.setField(key, pathValue);
          setStatus(`Filled in: ${pathValue}`);
        }
      })();
    })
      // The unlisten handle can arrive after the effect has already been cleaned up (StrictMode
      // double-mounts in development); dropping it there would leak a second live handler
      .then((unlisten) => { if (disposed) unlisten(); else dispose = unlisten; })
      .catch((error) => {
        if (!disposed) setStatus(`Desktop drag and drop is unavailable: ${error}`, 'err');
      });
    return () => { disposed = true; dispose?.(); };
  }, [changeRootDraft, pushHistory, setStatus]);

  useEffect(() => {
    if (!selectedJob) { setJobConfiguration(null); return; }
    let live = true;
    ipc.getJob(selectedJob.name).then((detail) => {
      if (live) setJobConfiguration(detail.job);
    }).catch((error) => {
      if (!live) return;
      setJobConfiguration(null);
      setStatus(`Failed to load '${selectedJob.name}' settings: ${error}`, 'err');
    });
    return () => { live = false; };
  }, [selectedJob, setStatus]);

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
    const clearLocalTicket = () => {
      if (autoScanTicket.current?.generation === ticket.generation
        && autoScanTicket.current.ticketId === ticket.ticketId) {
        autoScanTicket.current = null;
      }
    };
    const markCompleted = () => {
      autoScanLedger.current.markCompleted(ticket);
      clearLocalTicket();
    };
    const recoverOrDecline = async (reason: string) => {
      let observed: AutoScanStatusDto;
      try {
        observed = await ipc.autoScanStatus();
        acceptAutoScanStatus(observed, 'snapshot');
        observed = autoScanStatusRef.current ?? observed;
      } catch (error) {
        autoScanLedger.current.markDeclineRecovery(ticket);
        clearLocalTicket();
        setStatus(`${reason}; AutoScan could not verify whether the pending ticket needs release: ${error}`, 'err');
        return;
      }
      if (statusCompletesAutoScanTicket(observed, ticket)) {
        markCompleted();
        return;
      }
      if (!statusCanOwnAutoScanTrigger(observed, ticket)) {
        markCompleted();
        return;
      }
      try {
        const declined = await ipc.declineAutoScanTrigger(ticket.generation, ticket.ticketId);
        acceptAutoScanStatus(declined, 'decline', ticket);
        const authoritative = autoScanStatusRef.current ?? declined;
        if (statusCompletesAutoScanTicket(authoritative, ticket)) {
          markCompleted();
          setStatus(`${reason}; this AutoScan cycle was released without launching another Compare`, 'err');
          return;
        }
        if (!statusCanOwnAutoScanTrigger(authoritative, ticket)) {
          markCompleted();
          return;
        }
      } catch (declineError) {
        try {
          const recovered = await ipc.autoScanStatus();
          acceptAutoScanStatus(recovered, 'snapshot');
          const authoritative = autoScanStatusRef.current ?? recovered;
          if (statusCompletesAutoScanTicket(authoritative, ticket)) {
            markCompleted();
            return;
          }
          if (!statusCanOwnAutoScanTrigger(authoritative, ticket)) {
            markCompleted();
            return;
          }
        } catch (statusError) {
          autoScanLedger.current.markDeclineRecovery(ticket);
          clearLocalTicket();
          setStatus(`${reason}; decline failed (${declineError}) and recovery status failed (${statusError})`, 'err');
          return;
        }
        autoScanLedger.current.markDeclineRecovery(ticket);
        clearLocalTicket();
        setStatus(`${reason}; the ticket is still backend-owned and will be declined from a recovered trigger: ${declineError}`, 'err');
        return;
      }
      autoScanLedger.current.markDeclineRecovery(ticket);
      clearLocalTicket();
      setStatus(`${reason}; the decline response did not prove exact terminal ownership`, 'err');
    };

    const claim = autoScanLedger.current.claim(ticket);
    if (claim.kind === 'duplicate') return;
    if (claim.kind === 'capacity') {
      await recoverOrDecline('AutoScan rejected a trigger because its bounded recovery ledger is full');
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
    if (claim.kind === 'decline_recovery') {
      await recoverOrDecline('AutoScan recovered a ticket whose earlier decline was not confirmed');
      return;
    }

    let monitor = autoScanStatusRef.current;
    if (!statusCanOwnAutoScanTrigger(monitor, ticket)) {
      try {
        const snapshot = await ipc.autoScanStatus();
        acceptAutoScanStatus(snapshot, 'snapshot');
        monitor = autoScanStatusRef.current;
      } catch (error) {
        setStatus(`AutoScan could not verify trigger ownership: ${error}`, 'err');
      }
    }
    if (!statusCanOwnAutoScanTrigger(monitor, ticket)) {
      await recoverOrDecline('AutoScan refused a trigger that no longer had exact backend ownership');
      return;
    }

    autoScanTicket.current = ticket;
    const completion = await doCompare(ticket);
    if (!completion) {
      await recoverOrDecline('AutoScan Compare did not publish a result');
      return;
    }

    let publishedStatus: AutoScanStatusDto;
    try {
      publishedStatus = await ipc.autoScanStatus();
      acceptAutoScanStatus(publishedStatus, 'snapshot');
      publishedStatus = autoScanStatusRef.current ?? publishedStatus;
    } catch (error) {
      markCompleted();
      setStatus(`AutoScan retained the Compare result but could not verify its terminal status, so AutoApply was not attempted: ${error}`, 'err');
      return;
    }
    const ownsPublishedResult = monitorOwnsAutoScanResult(
      publishedStatus,
      autoScanTicket.current,
      ticket,
      completion.plan.owner,
    );
    const terminalStatus = statusCompletesAutoScanTicket(publishedStatus, ticket);
    markCompleted();
    if (!terminalStatus) {
      setStatus('AutoScan published a result, but its status cursor moved before AutoApply ownership could be proven; review the retained result', 'err');
      return;
    }
    if (!ownsPublishedResult) return;

    const freshPlan = completion.plan;
    if (freshPlan.ops.length === 0) return;
    if (!ticket.autoApply) {
      setStatus(`AutoScan found ${freshPlan.ops.length} differences — review required`, 'err');
      return;
    }

    const interaction = liveInteractionState.current;
    if (compareInFlight.current
      || applyExecutionRequest.current
      || applyReviewRequest.current
      || compareReviewRequest.current
      || interactionBlocksUnattendedWrite(interaction)) {
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
      if (interactionConflictsWithReservedWrite(liveInteractionState.current)
        || compareInFlight.current
        || applyExecutionRequest.current
        || applyReviewRequest.current
        || compareReviewRequest.current) {
        setStatus('AutoApply did not run because another interaction opened during authorization; review the retained result', 'err');
        return;
      }
      try {
        const result = await ipc.applyJob(authorization.authorization_token);
        refreshLatestRunSummaries();
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

  useEffect(() => {
    let disposed = false;
    let remove: (() => void) | null = null;
    void listen<CompareScopeExecutionStatusDto>('compare-execution-status', ({ payload }) => {
      if (!disposed) dispatchCompareWorkspace({ type: 'execution_status_received', execution: payload });
    }).then((unlisten) => {
      if (disposed) unlisten(); else remove = unlisten;
    }).catch((error) => {
      if (!disposed) setStatus(`Compare execution-status subscription failed: ${error}`, 'err');
    });
    return () => {
      disposed = true;
      remove?.();
    };
  }, [setStatus]);

  useInteractionLayer({
    kind: 'application',
    handlers: {
      compare: () => { void doCompare(); },
      synchronize: () => { void openConfirm(); },
      zoom_in: zoom.zoomIn,
      zoom_out: zoom.zoomOut,
      zoom_reset: zoom.zoomReset,
    },
  });
  useInteractionLayer({
    active: rootDraftOpen,
    kind: 'auxiliary_panel',
    handlers: {},
  });

  const hasDifferences = !!plan && plan.ops.length > 0;
  const reviewBusy = operationReviewPending(compareReview) || operationReviewPending(applyReview);
  const autoScanVerificationPending = autoScanStatus?.active === true
    && autoScanStatus.active_ticket !== null;
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
      const operation = effectiveOperation(plan, reversedRows, index);
      if (operation.action === 'copy') {
        totals.copyCount++;
        totals.transferBytes += rowTransferBytes(plan, reversedRows, index);
      } else if (operation.action === 'update') {
        totals.updateCount++;
        totals.transferBytes += rowTransferBytes(plan, reversedRows, index);
      } else if (operation.action === 'chmod') totals.updateCount++;
      else if (operation.action === 'move') totals.moveCount++;
      else if (operation.action === 'delete' || operation.action === 'delete_dir') {
        totals.deleteCount++;
        totals.deletionBytes += operation.size ?? 0;
      }
      if (reversedRows[index]) totals.reversedCount++;
    }
    totals.checkedOutsideScope = includedRows.filter(Boolean).length - executableIndices.length;
    return totals;
  }, [plan, executableIndices, reversedRows, includedRows]);

  return (
    <>
      <div className="app">
        <Sidebar
          jobs={jobs}
          currentJobId={selectedJob?.job_id ?? null}
          latestRunByJobId={latestRunByJobId}
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
            job={selectedJob}
            hasPlan={!!plan}
            executableCount={executableIndices.length}
            stats={stats}
            busy={busy || reviewBusy || autoScanVerificationPending || rootDraftOpen}
            canSync={applyAvailability.available}
            applyBlockedMessage={applyAvailability.blockedMessage}
            autoScanStatus={autoScanStatus}
            autoScanControlPending={autoScanControlPending}
            onCompare={() => void doCompare()}
            onSync={() => void openConfirm()}
            onToggleLog={() => setLogOpen((v) => !v)}
            onToggleAutoScan={() => {
              const action = autoScanToggleAction(autoScanStatusRef.current, selectedJob !== null);
              if (action === 'stop') { stopAutoScan(); return; }
              if (action !== 'start' || !selectedJob || autoScanControlPendingRef.current !== null) return;
              const monitoredJob = selectedJob;
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
            job={selectedJob}
            rootEditor={selectedRootEditor}
            jobConfiguration={jobConfiguration}
            busy={busy}
            reviewing={reviewBusy}
            selectedTargetIndex={selectedTargetIndex}
            pathHistory={pathHistory}
            dropTargetKey={dropTargetKey === 'source' || dropTargetKey === 'target' ? dropTargetKey : null}
            scopeRef={setPathScope}
            onDraftChange={changeRootDraft}
            onSave={(field) => void saveRootDraft(field)}
            onRevert={revertRootDraft}
            onAcceptConflict={acceptRootDraftConflict}
            onBrowse={(which) => void browseRoot(which)}
            onSwap={() => void requestSwap()}
            onSelectTarget={(targetIndex) => {
              if (busy || targetIndex === selectedTargetIndex) return;
              const selected = selectedJob;
              if (!selected) return;
              selectionRef.current = { job: selected, targetIndex };
              setSelectedTargetIndex(targetIndex);
              resetSafetyUi();
              const targetPath = selected.targets[targetIndex] ?? '';
              const restored = activeWorkspace(compareWorkspaceRepository, selected, targetIndex);
              setStatus(restored
                ? `Switched target → ${targetPath} · restored ${restored.plan.ops.length} compare items`
                : `Switched target → ${targetPath} — Compare again (Ctrl+R)`);
              requestResultRestore(selected, targetIndex, restored);
            }}
            onEditGroup={(g) => { if (selectedJob && !busy) setEditor({ name: selectedJob.name, focusGroup: g }); }}
          />
          {selectedScopeWorkspace?.candidate && selectedCompareWorkspace && (
            <CompareCandidateNotice
              candidate={selectedScopeWorkspace.candidate}
              activeHasReviewEdits={workspaceHasReviewEdits(selectedCompareWorkspace)}
              onAdopt={() => {
                const decision = {
                  scopeKey: selectedScopeWorkspace.key,
                  resultKey: selectedScopeWorkspace.candidate!.workspace.key,
                };
                if (workspaceHasReviewEdits(selectedCompareWorkspace)) {
                  setCandidateAdoption(decision);
                } else {
                  dispatchCompareWorkspace({
                    type: 'candidate_adopted',
                    scopeKey: decision.scopeKey,
                    expectedResultKey: decision.resultKey,
                  });
                  resetSafetyUi();
                }
              }}
              onDiscard={() => dispatchCompareWorkspace({
                type: 'candidate_discarded',
                scopeKey: selectedScopeWorkspace.key,
                expectedResultKey: selectedScopeWorkspace.candidate!.workspace.key,
              })}
            />
          )}
          {selectedCompareWorkspace && selectedScopeWorkspace && (
            <CompareActivityNotice
              activity={selectedScopeWorkspace.activity}
              workspaceExecutionAccess={workspaceExecutionAccess}
            />
          )}
          {selectedCompareWorkspace && workspaceExecutionAccess && (
            <CompareExecutionNotice access={workspaceExecutionAccess} execution={selectedScopeWorkspace?.execution ?? null} />
          )}
          {plan && !compareActive && (
            <ResultBar
              plan={plan}
              resultView={resultView}
              onResultViewChange={(next) => {
                if (selectedCompareWorkspace) {
                  dispatchCompareWorkspace({
                    type: 'result_view_changed',
                    resultKey: selectedCompareWorkspace.key,
                    view: next,
                  });
                }
                if (next === 'identical') setAdvancedFiltersAnchor(null);
              }}
              searchDraft={searchDraft}
              searchPending={searchPending}
              scopeCalculationPending={scopeCalculationPending}
              scopeCalculationFailed={scopeCalculationFailed}
              onSearchDraftChange={workspaceController.changeDifferenceSearchDraft}
              onClearSearch={() => workspaceController.changeDifferenceSearchDraft('')}
              scope={{
                foundCount: plan.ops.length,
                inScopeCount: inScopeIndices.length,
                selectedCount: executableIndices.length,
                folderScope,
                selectedResultTypes: [...selectedResultTypes],
                advancedFilterCount: countActiveAdvancedFilterGroups(appliedAdvancedFilter),
              }}
              onClearScope={clearRunScope}
              onClearFolderScope={() => {
                if (selectedCompareWorkspace) dispatchCompareWorkspace({
                  type: 'folder_scope_changed',
                  resultKey: selectedCompareWorkspace.key,
                  folderScope: null,
                });
              }}
              onClearSelectedResultTypes={() => {
                if (selectedCompareWorkspace) dispatchCompareWorkspace({
                  type: 'selected_result_types_changed',
                  resultKey: selectedCompareWorkspace.key,
                  resultTypes: EMPTY_RESULT_TYPES,
                });
              }}
              onClearAdvancedFilters={clearAdvancedFilters}
              advancedFiltersOpen={!!advancedFiltersAnchor}
              onToggleAdvancedFilters={(anchor) => setAdvancedFiltersAnchor((current) => (current ? null : anchor))}
              exportPending={csvExportPending}
              onExportCsv={() => void exportCsv()}
              grouped={grouped}
              sort={sort}
              anyCollapsed={anyCollapsed}
              pathMode={pathMode}
              onToggleFold={() => {
                if (selectedCompareWorkspace) dispatchCompareWorkspace({
                  type: 'difference_folds_replaced',
                  resultKey: selectedCompareWorkspace.key,
                  collapsedFolders: anyCollapsed ? EMPTY_PATH_SET : new Set(folderPathsInLayout),
                });
              }}
              onToggleGroup={() => {
                const next = !grouped;
                if (selectedCompareWorkspace) dispatchCompareWorkspace({
                  type: 'difference_grouping_changed',
                  resultKey: selectedCompareWorkspace.key,
                  grouped: next,
                });
                const preferences = { ...workspacePreferences, grouped: next };
                setWorkspacePreferences(preferences);
                persistWorkspacePreferences(preferences);
              }}
              onClearSort={() => {
                if (selectedCompareWorkspace) dispatchCompareWorkspace({
                  type: 'difference_sort_changed',
                  resultKey: selectedCompareWorkspace.key,
                  sort: null,
                });
              }}
              onTogglePathMode={() => {
                const next: CompareWorkspacePreferences['pathMode'] = pathMode === 'relative' ? 'full' : 'relative';
                if (selectedCompareWorkspace) dispatchCompareWorkspace({
                  type: 'path_mode_changed',
                  resultKey: selectedCompareWorkspace.key,
                  pathMode: next,
                });
                const preferences = { ...workspacePreferences, pathMode: next };
                setWorkspacePreferences(preferences);
                persistWorkspacePreferences(preferences);
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
                reversedRows={reversedRows}
                selectedResultTypes={selectedResultTypes}
                onSelectedResultTypesChange={(resultTypes) => {
                  if (selectedCompareWorkspace) dispatchCompareWorkspace({
                    type: 'selected_result_types_changed',
                    resultKey: selectedCompareWorkspace.key,
                    resultTypes,
                  });
                }}
                folderScope={folderScope}
                onFolderScopeChange={(nextFolderScope) => {
                  if (selectedCompareWorkspace) dispatchCompareWorkspace({
                    type: 'folder_scope_changed',
                    resultKey: selectedCompareWorkspace.key,
                    folderScope: nextFolderScope,
                  });
                }}
                collapsed={panelCollapsed}
                expandedFolders={expandedFolders}
                onToggleCollapsed={() => {
                  if (!selectedCompareWorkspace) return;
                  const collapsed = !panelCollapsed;
                  dispatchCompareWorkspace({
                    type: 'scope_panel_collapsed_changed',
                    resultKey: selectedCompareWorkspace.key,
                    collapsed,
                  });
                  const preferences = { ...workspacePreferences, scopePanelCollapsed: collapsed };
                  setWorkspacePreferences(preferences);
                  persistWorkspacePreferences(preferences);
                }}
                onToggleExpandedFolder={(folderPath) => {
                  if (selectedCompareWorkspace) dispatchCompareWorkspace({
                    type: 'scope_folder_expansion_toggled',
                    resultKey: selectedCompareWorkspace.key,
                    folderPath,
                  });
                }}
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
              ref={setResultViewportElement}
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
                    ipc.cancelCompareRun(runId).then((accepted) => {
                      if (accepted) return;
                      setCompareCancelling(false);
                      setStatus('That compare already finished; no newer run was cancelled');
                    }).catch((e) => {
                      setCompareCancelling(false);
                      setStatus(`Cancel failed: ${e}`, 'err');
                    });
                  }}
                />
              ) : resultView === 'identical' && selectedCompareWorkspace ? (
                <IdenticalResultsPanel
                  workspace={selectedCompareWorkspace.identical}
                  onSearchDraftChange={workspaceController.changeIdenticalSearchDraft}
                  onLoadMore={workspaceController.loadMoreIdentical}
                  onRetry={workspaceController.retryIdentical}
                />
              ) : hasDifferences ? (
                <PlanTable
                  plan={plan}
                  reversedRows={reversedRows}
                  includedRows={includedRows}
                  rowPlan={rowPlan}
                  displayOrder={layout.displayOrder}
                  inScopeIndices={inScopeIndices}
                  pathMode={pathMode}
                  grouped={grouped}
                  sort={sort}
                  collapsedFolderPaths={collapsedFolderPaths}
                  workspaceKey={selectedCompareWorkspace!.key}
                  viewport={selectedCompareWorkspace!.differences.viewport}
                  reviewEditable={reviewEditable}
                  wrap={resultViewportElement}
                  onSetRowIncluded={setRowIncluded}
                  onSetRowsIncluded={setRowsIncluded}
                  onToggleRowDirection={toggleRowDirection}
                  onToggleFolderFold={toggleFolderFold}
                  onSort={toggleSort}
                  onContextRow={rowMenu}
                  onViewportChange={recordDifferenceViewport}
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
                  title={selectedJob ? `Ready — ${selectedJob.name}` : 'No Job Selected'}
                  description={
                    selectedJob
                      ? 'Press Compare (F5 or Ctrl+R) to walk both roots and build a plan.'
                      : 'Pick a job on the left, then press Compare (F5 or Ctrl+R).'
                  }
                />
              )}
            </div>
          </div>
          {logOpen && (
            <LogPanel
              jobId={selectedJob?.job_id ?? null}
              reloadKey={logReload}
              onClose={() => setLogOpen(false)}
              onSettings={() => setSettingsOpen(true)}
              onStatus={setStatus}
            />
          )}
          <StatusBar
            status={status}
            onAction={runStatusAction}
            onDismissNotice={dismissStatusNotice}
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
      {advancedFiltersAnchor && plan && selectedCompareWorkspace && (
        <AdvancedFiltersPopover
          key={selectedCompareWorkspace.key}
          anchor={advancedFiltersAnchor}
          appliedFilter={appliedAdvancedFilter}
          inScopeCount={inScopeIndices.length}
          differenceCount={plan.ops.length}
          onApplyFilter={workspaceController.applyAdvancedFilter}
          onWriteValidatedFilterMasksToJobExclude={(validatedFilter) => {
            void addExcludes(validatedFilter.masks, 'Written into the exclude list');
          }}
          onDismiss={() => setAdvancedFiltersAnchor(null)}
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
            const selectedIdentity = !!original && selectedJob?.job_id === original.jobId;
            const semanticMutation = !!original
              && saved.config_revision !== original.configRevision;
            setEditor(null);
            reconcileSavedWorkspaceJob(saved, original);
            if (selectedIdentity && semanticMutation) resetSafetyUi();
            pushHistory(job.source);
            for (const target of job.targets) pushHistory(target);
            const warning = statusDeliveryWarning(saved);
            try {
              await refreshJobs();
              if (selectedIdentity) setJobConfiguration(job);
              setStatus(
                (saved.effect === 'no_op' ? `No changes to save for '${saved.name}'` : `Saved '${saved.name}'`) + warning,
                warning ? 'err' : 'ok',
              );
            } catch (error) {
              setStatus(`Saved '${saved.name}'${warning} · job-list refresh failed: ${error}`, 'err');
            }
          }}
          onDeleted={async (deleted) => {
            setEditor(null);
            expireDeletedJobState(deleted.job_id);
            if (selectedJob?.job_id === deleted.job_id) {
              resetSafetyUi();
              setRegistrySelection(null);
              setSelectedTargetIndex(0);
            }
            const warning = statusDeliveryWarning(deleted);
            try {
              await refreshJobs();
              setStatus(`Deleted '${deleted.name}'${warning}`, warning ? 'err' : '');
            } catch (error) {
              setStatus(`Deleted '${deleted.name}'${warning} · job-list refresh failed: ${error}`, 'err');
            }
          }}
          onMutationConflict={async (name, original) => {
            const list = await refreshJobs();
            if (!original) return;
            const refreshed = list.find((candidate) => candidate.job_id === original.jobId) ?? null;
            reconcileWorkspaceJob(original, refreshed);
            if (selectedJob?.job_id === original.jobId
              && (!refreshed || selectedJob.config_revision !== refreshed.config_revision)) {
              resetSafetyUi();
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
      {candidateAdoption && (
        <ConfirmDialog
          title="Review the newer AutoScan result?"
          message={
            'The newer exact result will replace this active review workspace. Your current row decisions, filters, ' +
            'folds, and scroll position do not carry into different filesystem evidence.'
          }
          actions={[{
            label: 'Review New Result',
            onConfirm: () => {
              const scope = compareWorkspaceRepository.scopes.find((entry) => entry.key === candidateAdoption.scopeKey);
              if (scope?.candidate?.workspace.key !== candidateAdoption.resultKey) {
                setStatus('The AutoScan candidate changed — review the newer candidate before adopting it', 'err');
                setCandidateAdoption(null);
                return;
              }
              dispatchCompareWorkspace({
                type: 'candidate_adopted',
                scopeKey: candidateAdoption.scopeKey,
                expectedResultKey: candidateAdoption.resultKey,
              });
              setCandidateAdoption(null);
              resetSafetyUi();
            },
          }]}
          onCancel={() => setCandidateAdoption(null)}
        />
      )}
      {askSwap && (
        <ConfirmDialog
          title={`Swap the two roots of '${askSwap.owner.jobName}'?`}
          message={
            `source ← ${askSwap.values.target}\n` +
            `target ${askSwap.owner.targetIndex + 1} ← ${askSwap.values.source}\n\n` +
            (askSwap.mode === 'mirror'
              ? 'In mirror mode this reverses which side is authoritative: after the swap, the original target wins.\n\n'
              : '') +
            'The job file is rewritten atomically. Existing Compare evidence remains viewable, but cannot be applied after the configuration changes. The status bar keeps an undo.'
          }
          actions={[{
            label: 'Swap them',
            onConfirm: () => void doSwap(askSwap),
          }]}
          onCancel={() => setAskSwap(null)}
        />
      )}
      {confirmOpen && selectedJob && confirmTotals && (
        <ConfirmSheet
          job={selectedJob}
          totals={confirmTotals}
          reviewState={applyReview}
          choices={applyChoices}
          onChoices={setApplyChoices}
          onCancel={resetConfirmation}
          onConfirm={() => void doSync()}
          onReviewAgain={() => {
            resetConfirmation();
            void openConfirm();
          }}
        />
      )}
      {compareReview.phase !== 'idle' && compareReview.phase !== 'authorized' && (
        <CompareReviewSheet
          state={compareReview}
          choices={compareChoices}
          onChoices={setCompareChoices}
          onCancel={resetCompareReview}
          onApprove={() => void approveCompareReview()}
          onReviewAgain={() => {
            resetCompareReview();
            void doCompare();
          }}
        />
      )}
    </>
  );
}
