import { useCallback, useEffect, useLayoutEffect, useRef } from 'react';
import { listIdentical, maskMatch } from '../../core/ipc';
import { effectiveOperation } from '../../core/plan';
import { isMaskMatchResult } from '../../core/runScope';
import {
  type CompareWorkspace,
} from '../state/compareWorkspaceModel';
import { advancedFilterMasksEqual } from '../state/compareWorkspaceFilters';
import type { CompareWorkspaceAction } from '../state/compareWorkspaceRepository';
import type { AdvancedScopeFilter } from '../../core/runScope';
import type { Dispatch, MutableRefObject } from 'react';

const DIFFERENCE_SEARCH_DELAY_MS = 150;
const IDENTICAL_SEARCH_DELAY_MS = 250;
const IDENTICAL_PAGE_SIZE = 300;

function nextRequestId(sequence: MutableRefObject<number>, floor: number): number {
  const next = Math.max(sequence.current + 1, floor + 1);
  if (!Number.isSafeInteger(next)) throw new Error('Compare workspace request IDs are exhausted');
  sequence.current = next;
  return next;
}

export function useCompareWorkspaceController(
  workspace: CompareWorkspace | null,
  dispatch: Dispatch<CompareWorkspaceAction>,
  reportError: (message: string) => void,
) {
  const requestSequence = useRef(0);
  const resultKey = workspace?.key ?? null;
  const activeResultKey = useRef(resultKey);
  useLayoutEffect(() => {
    activeResultKey.current = resultKey;
  }, [resultKey]);
  const latestMaskRequest = useRef<{
    resultKey: CompareWorkspace['key'];
    inputRevision: number;
    requestId: number;
  } | null>(null);
  const latestIdenticalRequest = useRef<{
    resultKey: CompareWorkspace['key'];
    query: string;
    requestId: number;
    offset: number;
  } | null>(null);
  const differenceSearchDraft = workspace?.differences.searchDraft ?? '';
  const appliedDifferenceSearch = workspace?.differences.appliedSearch ?? '';
  const differenceSearchRequestId = workspace?.differences.searchRequestId ?? 0;
  const appliedAdvancedFilter = workspace?.differences.appliedAdvancedFilter ?? null;
  const maskResolution = workspace?.differences.maskResolution ?? null;
  const maskInputRevision = workspace?.differences.maskInputRevision ?? 0;
  const rowReversed = workspace?.differences.rowReversed ?? null;
  const plan = workspace?.plan ?? null;
  const identicalSearchDraft = workspace?.identical.searchDraft ?? '';
  const appliedIdenticalSearch = workspace?.identical.appliedSearch ?? '';
  const identicalSearchRequestId = workspace?.identical.searchRequestId ?? 0;

  const changeDifferenceSearchDraft = useCallback((draft: string) => {
    if (!resultKey) return;
    dispatch({
      type: 'difference_search_draft_changed',
      resultKey,
      requestId: nextRequestId(requestSequence, differenceSearchRequestId),
      draft,
    });
  }, [differenceSearchRequestId, dispatch, resultKey]);

  const applyAdvancedFilter = useCallback((appliedFilter: AdvancedScopeFilter) => {
    if (!resultKey || !appliedAdvancedFilter) return;
    if (!advancedFilterMasksEqual(appliedAdvancedFilter, appliedFilter)) latestMaskRequest.current = null;
    dispatch({ type: 'advanced_filter_applied', resultKey, appliedFilter });
  }, [appliedAdvancedFilter, dispatch, resultKey]);

  const changeIdenticalSearchDraft = useCallback((draft: string) => {
    if (!resultKey) return;
    latestIdenticalRequest.current = null;
    dispatch({
      type: 'identical_search_draft_changed',
      resultKey,
      requestId: nextRequestId(requestSequence, identicalSearchRequestId),
      draft,
    });
  }, [dispatch, identicalSearchRequestId, resultKey]);

  useEffect(() => {
    if (!resultKey) return;
    const query = differenceSearchDraft.trim();
    if (query === appliedDifferenceSearch) return;
    const requestId = differenceSearchRequestId;
    const timer = window.setTimeout(() => {
      dispatch({ type: 'difference_search_applied', resultKey, requestId, query });
    }, DIFFERENCE_SEARCH_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [
    appliedDifferenceSearch,
    differenceSearchDraft,
    differenceSearchRequestId,
    dispatch,
    resultKey,
  ]);

  useEffect(() => {
    if (!resultKey
      || !plan
      || !rowReversed
      || maskResolution?.status !== 'unresolved'
      || !appliedAdvancedFilter
    ) return;
    const inputRevision = maskInputRevision;
    const requestId = nextRequestId(requestSequence, 0);
    const paths = plan.ops.map((_, index) => (
      effectiveOperation(plan, rowReversed, index).path
    ));
    const masks = appliedAdvancedFilter.masks;
    latestMaskRequest.current = { resultKey, inputRevision, requestId };
    dispatch({ type: 'mask_resolution_started', resultKey, inputRevision, requestId });
    void maskMatch(masks, paths).then((excludedByRow) => {
      if (!isMaskMatchResult(excludedByRow, paths.length)) {
        throw new Error('mask matching returned a result that does not align with the compared rows');
      }
      dispatch({
        type: 'mask_resolution_succeeded',
        resultKey,
        inputRevision,
        requestId,
        excludedByRow,
      });
    }).catch((error) => {
      const message = String(error);
      const latest = latestMaskRequest.current;
      const shouldReport = activeResultKey.current === resultKey
        && latest?.resultKey === resultKey
        && latest.inputRevision === inputRevision
        && latest.requestId === requestId;
      dispatch({ type: 'mask_resolution_failed', resultKey, inputRevision, requestId, error: message });
      if (shouldReport) {
        reportError(`Mask matching failed; this result remains view-only until the filter is revised: ${message}`);
      }
    });
  }, [
    appliedAdvancedFilter,
    dispatch,
    maskInputRevision,
    maskResolution?.status,
    plan,
    reportError,
    resultKey,
    rowReversed,
  ]);

  useEffect(() => {
    if (!resultKey) return;
    const query = identicalSearchDraft.trim();
    if (query === appliedIdenticalSearch) return;
    const requestId = identicalSearchRequestId;
    const timer = window.setTimeout(() => {
      dispatch({ type: 'identical_search_applied', resultKey, requestId, query });
    }, IDENTICAL_SEARCH_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [
    appliedIdenticalSearch,
    dispatch,
    identicalSearchDraft,
    identicalSearchRequestId,
    resultKey,
  ]);

  const loadIdenticalInitial = useCallback((target: CompareWorkspace) => {
    const resultKey = target.key;
    const query = target.identical.appliedSearch;
    const requestId = nextRequestId(requestSequence, 0);
    latestIdenticalRequest.current = { resultKey, requestId, query, offset: 0 };
    dispatch({ type: 'identical_initial_load_started', resultKey, requestId, query });
    void listIdentical(target.identity, query, 0, IDENTICAL_PAGE_SIZE).then((page) => {
      dispatch({
        type: 'identical_page_loaded',
        resultKey,
        requestId,
        query,
        offset: 0,
        rows: page.rows,
        total: page.total,
      });
    }).catch((error) => {
      const message = String(error);
      const latest = latestIdenticalRequest.current;
      const shouldReport = activeResultKey.current === resultKey
        && latest?.resultKey === resultKey
        && latest.requestId === requestId
        && latest.query === query
        && latest.offset === 0;
      dispatch({ type: 'identical_page_failed', resultKey, requestId, query, error: message });
      if (shouldReport) reportError(`Identical results could not be loaded: ${message}`);
    });
  }, [dispatch, reportError]);

  useEffect(() => {
    if (!workspace || workspace.selectedView !== 'identical' || workspace.identical.pages.status !== 'idle') return;
    loadIdenticalInitial(workspace);
  }, [loadIdenticalInitial, workspace]);

  const loadMoreIdentical = useCallback(() => {
    if (!workspace) return;
    const pages = workspace.identical.pages;
    if (pages.status !== 'ready' && pages.status !== 'load_more_failed') return;
    if (pages.rows.length >= pages.total) return;
    const resultKey = workspace.key;
    const query = workspace.identical.appliedSearch;
    const offset = pages.rows.length;
    const requestId = nextRequestId(requestSequence, 0);
    latestIdenticalRequest.current = { resultKey, requestId, query, offset };
    dispatch({ type: 'identical_load_more_started', resultKey, requestId, query, offset });
    void listIdentical(workspace.identity, query, offset, IDENTICAL_PAGE_SIZE).then((page) => {
      dispatch({
        type: 'identical_page_loaded',
        resultKey,
        requestId,
        query,
        offset,
        rows: page.rows,
        total: page.total,
      });
    }).catch((error) => {
      const message = String(error);
      const latest = latestIdenticalRequest.current;
      const shouldReport = activeResultKey.current === resultKey
        && latest?.resultKey === resultKey
        && latest.requestId === requestId
        && latest.query === query
        && latest.offset === offset;
      dispatch({ type: 'identical_page_failed', resultKey, requestId, query, error: message });
      if (shouldReport) reportError(`More identical results could not be loaded: ${message}`);
    });
  }, [dispatch, reportError, workspace]);

  const retryIdentical = useCallback(() => {
    if (!workspace || workspace.identical.pages.status !== 'initial_failed') return;
    loadIdenticalInitial(workspace);
  }, [loadIdenticalInitial, workspace]);

  return {
    changeDifferenceSearchDraft,
    applyAdvancedFilter,
    changeIdenticalSearchDraft,
    loadMoreIdentical,
    retryIdentical,
  };
}
