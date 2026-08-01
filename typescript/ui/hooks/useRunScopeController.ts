import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { maskMatch } from '../../core/ipc';
import { effectiveOperation } from '../../core/plan';
import type { PlanDto, ResultType } from '../../core/plan';
import {
  EMPTY_ADVANCED_SCOPE_FILTER,
  isMaskMatchResult,
  matchesFolderScope,
  parseScopeMasks,
} from '../../core/runScope';
import type { AdvancedScopeFilter } from '../../core/runScope';
import { RequestFence } from '../state/request-fence';
import {
  readRunScopePanelCollapsed,
  writeRunScopePanelCollapsed,
} from '../state/result-workspace';

const SEARCH_DELAY_MS = 150;
const MASK_DELAY_MS = 200;

type MaskStatus = 'idle' | 'pending' | 'ready' | 'failed';

interface MaskResolution {
  plan: PlanDto;
  flipped: boolean[];
  maskSignature: string;
  status: 'ready' | 'failed';
  excluded: boolean[];
}

export function useRunScopeController(
  plan: PlanDto | null,
  flipped: boolean[],
  onError: (message: string) => void,
) {
  const [selectedResultTypes, setSelectedResultTypes] = useState<Set<ResultType>>(new Set());
  const [searchDraft, setSearchDraft] = useState('');
  const [searchQuery, setSearchQuery] = useState('');
  const [folderScope, setFolderScope] = useState<string | null>(null);
  const [advancedFilter, setAdvancedFilter] = useState<AdvancedScopeFilter>(EMPTY_ADVANCED_SCOPE_FILTER);
  const [maskDraft, setMaskDraft] = useState('');
  const [expandedFolders, setExpandedFolders] = useState<Set<string>>(new Set());
  const [panelCollapsed, setPanelCollapsed] = useState(() => readRunScopePanelCollapsed(localStorage));
  const [maskResolution, setMaskResolution] = useState<MaskResolution | null>(null);
  const maskRequest = useRef(new RequestFence());

  const searchPending = searchDraft.trim() !== searchQuery;
  const maskSignature = advancedFilter.masks.join('\n');
  const maskDraftSignature = parseScopeMasks(maskDraft).join('\n');
  const maskDraftPending = maskDraftSignature !== maskSignature;
  const maskResolutionMatches = maskResolution?.plan === plan
    && maskResolution.flipped === flipped
    && maskResolution.maskSignature === maskSignature;
  const maskStatus: MaskStatus = advancedFilter.masks.length === 0
    ? 'idle'
    : maskResolutionMatches
      ? maskResolution.status
      : 'pending';
  const excludedByMask = useMemo(() => {
    if (advancedFilter.masks.length === 0 || !plan) return [];
    if (maskResolutionMatches && maskResolution.status === 'ready') return maskResolution.excluded;
    return plan.ops.map(() => true);
  }, [advancedFilter.masks.length, maskResolution, maskResolutionMatches, plan]);

  useEffect(() => {
    const nextQuery = searchDraft.trim();
    if (nextQuery === searchQuery) return undefined;
    const timer = window.setTimeout(() => setSearchQuery(nextQuery), SEARCH_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [searchDraft, searchQuery]);

  useEffect(() => {
    if (!maskDraftPending) return undefined;
    const timer = window.setTimeout(() => {
      const masks = parseScopeMasks(maskDraft);
      setAdvancedFilter((current) => (
        masks.join('\n') === current.masks.join('\n') ? current : { ...current, masks }
      ));
    }, MASK_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [maskDraft, maskDraftPending]);

  const planIdentity = plan
    ? `${plan.owner.compare_id}\0${plan.owner.job_id}\0${plan.owner.target_index}\0${plan.owner.config_revision}`
    : '';
  useEffect(() => {
    setSearchDraft(searchQuery);
  }, [planIdentity, searchQuery]);

  useEffect(() => {
    if (!plan || advancedFilter.masks.length === 0) {
      maskRequest.current.invalidate();
      setMaskResolution(null);
      return undefined;
    }
    const ticket = maskRequest.current.start(`${planIdentity}\0${maskSignature}`);
    const requestedPlan = plan;
    const requestedFlips = flipped;
    void maskMatch(
      advancedFilter.masks,
      plan.ops.map((_, index) => effectiveOperation(plan, flipped, index).path),
    ).then((excluded) => {
      if (!maskRequest.current.owns(ticket)) return;
      if (!isMaskMatchResult(excluded, requestedPlan.ops.length)) {
        throw new Error('mask matching returned a result that does not align with the compared rows');
      }
      setMaskResolution({
        plan: requestedPlan,
        flipped: requestedFlips,
        maskSignature,
        status: 'ready',
        excluded,
      });
    }).catch((error) => {
      if (!maskRequest.current.owns(ticket)) return;
      setMaskResolution({
        plan: requestedPlan,
        flipped: requestedFlips,
        maskSignature,
        status: 'failed',
        excluded: requestedPlan.ops.map(() => true),
      });
      onError(`Mask matching failed; the affected run scope remains blocked: ${error}`);
    });
    return () => maskRequest.current.invalidate();
  }, [advancedFilter.masks, flipped, maskSignature, onError, plan, planIdentity]);

  useEffect(() => {
    if (!plan || folderScope === null) return;
    const stillExists = plan.ops.some((_, index) => (
      matchesFolderScope(effectiveOperation(plan, flipped, index), folderScope)
    ));
    if (!stillExists) setFolderScope(null);
  }, [folderScope, flipped, plan]);

  const clearSearch = useCallback(() => {
    setSearchDraft('');
    setSearchQuery('');
  }, []);

  const clearAdvancedFilter = useCallback(() => {
    setAdvancedFilter(EMPTY_ADVANCED_SCOPE_FILTER);
    setMaskDraft('');
  }, []);

  const clearRunScope = useCallback(() => {
    setSelectedResultTypes(new Set());
    clearSearch();
    setFolderScope(null);
    clearAdvancedFilter();
  }, [clearAdvancedFilter, clearSearch]);

  const resetResultWorkspace = useCallback(() => {
    clearRunScope();
    setExpandedFolders(new Set());
  }, [clearRunScope]);

  const toggleExpandedFolder = useCallback((path: string) => {
    setExpandedFolders((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path); else next.add(path);
      return next;
    });
  }, []);

  const togglePanelCollapsed = useCallback(() => {
    setPanelCollapsed((collapsed) => {
      const next = !collapsed;
      writeRunScopePanelCollapsed(localStorage, next);
      return next;
    });
  }, []);

  return {
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
    clearAdvancedFilter,
    excludedByMask,
    maskStatus,
    scopeCalculationPending: searchPending || maskDraftPending || maskStatus === 'pending',
    scopeCalculationFailed: maskStatus === 'failed',
    clearRunScope,
    resetResultWorkspace,
    expandedFolders,
    toggleExpandedFolder,
    panelCollapsed,
    togglePanelCollapsed,
  };
}
