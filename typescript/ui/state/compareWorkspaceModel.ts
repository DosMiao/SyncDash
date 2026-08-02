// A retained Compare workspace is review evidence, not execution authority. While the renderer
// session is alive, this model keeps the exact result and its review state intact as execution
// eligibility changes independently.

import { isExecutableOperation } from '../../core/plan.ts';
import type { PlanDto, ResultType, Sort } from '../../core/plan.ts';
import { EMPTY_ADVANCED_SCOPE_FILTER } from '../../core/runScope.ts';
import type { AdvancedScopeFilter } from '../../core/runScope.ts';
import type { CompareIdentity } from '../../core/types/generated/CompareIdentity.ts';
import type { CompareScopeDto } from '../../core/types/generated/CompareScopeDto.ts';
import type { CompareScopeExecutionStatusDto } from '../../core/types/generated/CompareScopeExecutionStatusDto.ts';
import type { IdenticalRow } from '../../core/types/generated/IdenticalRow.ts';
import type { JobDto } from '../../core/types/generated/JobDto.ts';

export type CompareScopeKey = string & { readonly compareScopeKey: unique symbol };
export type CompareResultKey = string & { readonly compareResultKey: unique symbol };

export type ResultRetentionState =
  | { status: 'checking'; requestId: number }
  | { status: 'retained'; requestId: number }
  | { status: 'missing'; requestId: number }
  | { status: 'check_failed'; requestId: number; error: string };

export type MaskResolutionState =
  | { status: 'not_required'; inputRevision: number }
  | { status: 'unresolved'; inputRevision: number }
  | { status: 'pending'; inputRevision: number; requestId: number }
  | { status: 'ready'; inputRevision: number; requestId: number; excludedByRow: boolean[] }
  | { status: 'failed'; inputRevision: number; requestId: number; error: string };

export type IdenticalPageState =
  | { status: 'idle' }
  | { status: 'loading_initial'; requestId: number; query: string }
  | { status: 'ready'; query: string; rows: IdenticalRow[]; total: number }
  | {
    status: 'loading_more';
    requestId: number;
    query: string;
    offset: number;
    rows: IdenticalRow[];
    total: number;
  }
  | { status: 'initial_failed'; query: string; error: string }
  | { status: 'load_more_failed'; query: string; rows: IdenticalRow[]; total: number; error: string };

export interface DifferenceWorkspace {
  rowIncluded: boolean[];
  rowReversed: boolean[];
  selectedResultTypes: Set<ResultType>;
  searchDraft: string;
  appliedSearch: string;
  searchRequestId: number;
  folderScope: string | null;
  appliedAdvancedFilter: AdvancedScopeFilter;
  maskInputRevision: number;
  maskResolution: MaskResolutionState;
  expandedScopeFolders: Set<string>;
  scopePanelCollapsed: boolean;
  sort: Sort | null;
  grouped: boolean;
  pathMode: 'relative' | 'full';
  collapsedFolders: Set<string>;
  viewport: DifferenceViewport;
}

export interface DifferenceViewport {
  logicalTop: number;
  scrollLeft: number;
}

export interface IdenticalWorkspace {
  searchDraft: string;
  appliedSearch: string;
  searchRequestId: number;
  pages: IdenticalPageState;
  viewport: IdenticalViewport;
}

export interface IdenticalViewport {
  scrollTop: number;
  scrollLeft: number;
}

export type CompareResultView = 'differences' | 'identical';

export interface CompareWorkspace {
  key: CompareResultKey;
  identity: CompareIdentity;
  readonly plan: PlanDto;
  retention: ResultRetentionState;
  selectedView: CompareResultView;
  differences: DifferenceWorkspace;
  identical: IdenticalWorkspace;
  reviewRevision: number;
}

export interface CompareWorkspacePreferences {
  grouped: boolean;
  pathMode: 'relative' | 'full';
  scopePanelCollapsed: boolean;
}

export const defaultCompareWorkspacePreferences: CompareWorkspacePreferences = {
  grouped: true,
  pathMode: 'relative',
  scopePanelCollapsed: true,
};

export type CompareActivityOrigin =
  | { kind: 'interactive' }
  | { kind: 'auto_scan'; generation: number; ticketId: number };

export type AutoScanPublicationOrigin = Extract<CompareActivityOrigin, { kind: 'auto_scan' }>;

export interface StagedCompareCandidate {
  workspace: CompareWorkspace;
  origin: AutoScanPublicationOrigin;
}

export interface AutoScanPublicationCursor {
  generation: number;
  ticketId: number;
  resultKey: CompareResultKey;
}

export type CompareScopeActivity =
  | { status: 'idle'; requestId: number }
  | { status: 'comparing'; requestId: number; origin: CompareActivityOrigin }
  | { status: 'failed'; requestId: number; origin: CompareActivityOrigin; error: string };

export type ScopeRestorationState =
  | { status: 'idle'; requestId: number }
  | { status: 'loading'; requestId: number }
  | { status: 'failed'; requestId: number; error: string };

export interface CompareScopeWorkspace {
  key: CompareScopeKey;
  scope: CompareScopeDto;
  execution: CompareScopeExecutionStatusDto | null;
  active: CompareWorkspace | null;
  candidate: StagedCompareCandidate | null;
  dismissedCandidateKey: CompareResultKey | null;
  latestAutoScanPublication: AutoScanPublicationCursor | null;
  activity: CompareScopeActivity;
  restoration: ScopeRestorationState;
}

export interface CompareWorkspaceRepository {
  scopes: CompareScopeWorkspace[];
}

export const emptyCompareWorkspaceRepository: CompareWorkspaceRepository = { scopes: [] };

export function compareScopeKey(scope: CompareScopeDto): CompareScopeKey {
  return `${scope.job_id}\0${scope.target_index}\0${scope.config_revision}` as CompareScopeKey;
}

export function compareResultKey(identity: CompareIdentity): CompareResultKey {
  if (!/^[0-9a-f]{32}$/.test(identity.result_id)) {
    throw new Error('Compare result identity is not a canonical 128-bit hexadecimal value');
  }
  return identity.result_id as CompareResultKey;
}

export function compareScopeFromIdentity(identity: CompareIdentity): CompareScopeDto {
  return {
    job_id: identity.job_id,
    target_index: identity.target_index,
    config_revision: identity.config_revision,
  };
}

export function compareScopeForJob(job: JobDto, targetIndex: number): CompareScopeDto {
  return { job_id: job.job_id, target_index: targetIndex, config_revision: job.config_revision };
}

export function sameCompareIdentity(left: CompareIdentity, right: CompareIdentity): boolean {
  return left.result_id === right.result_id
    && left.compare_run_id === right.compare_run_id
    && left.job_id === right.job_id
    && left.target_index === right.target_index
    && left.config_revision === right.config_revision;
}

export function sameCompareScope(left: CompareScopeDto, right: CompareScopeDto): boolean {
  return left.job_id === right.job_id
    && left.target_index === right.target_index
    && left.config_revision === right.config_revision;
}

export function createCompareWorkspace(
  plan: PlanDto,
  preferences: CompareWorkspacePreferences = defaultCompareWorkspacePreferences,
): CompareWorkspace {
  if (plan.metas.length !== plan.ops.length) {
    throw new Error('Compare metadata must contain exactly one entry per operation');
  }
  return {
    key: compareResultKey(plan.owner.identity),
    identity: plan.owner.identity,
    plan,
    retention: { status: 'retained', requestId: 0 },
    selectedView: 'differences',
    differences: {
      rowIncluded: plan.ops.map(isExecutableOperation),
      rowReversed: plan.ops.map(() => false),
      selectedResultTypes: new Set(),
      searchDraft: '',
      appliedSearch: '',
      searchRequestId: 0,
      folderScope: null,
      appliedAdvancedFilter: { ...EMPTY_ADVANCED_SCOPE_FILTER, masks: [] },
      maskInputRevision: 0,
      maskResolution: { status: 'not_required', inputRevision: 0 },
      expandedScopeFolders: new Set(),
      scopePanelCollapsed: preferences.scopePanelCollapsed,
      sort: null,
      grouped: preferences.grouped,
      pathMode: preferences.pathMode,
      collapsedFolders: new Set(),
      viewport: { logicalTop: 0, scrollLeft: 0 },
    },
    identical: {
      searchDraft: '',
      appliedSearch: '',
      searchRequestId: 0,
      pages: { status: 'idle' },
      viewport: { scrollTop: 0, scrollLeft: 0 },
    },
    reviewRevision: 0,
  };
}

export function scopeWorkspace(
  repository: CompareWorkspaceRepository,
  scope: CompareScopeDto,
): CompareScopeWorkspace | null {
  return repository.scopes.find((entry) => entry.key === compareScopeKey(scope)) ?? null;
}

export function activeWorkspace(
  repository: CompareWorkspaceRepository,
  job: JobDto | null,
  targetIndex: number,
): CompareWorkspace | null {
  if (!job) return null;
  return scopeWorkspace(repository, compareScopeForJob(job, targetIndex))?.active ?? null;
}

export function preferredTargetIndex(repository: CompareWorkspaceRepository, job: JobDto): number {
  const entry = repository.scopes.find((candidate) => (
    candidate.active !== null
    && candidate.scope.job_id === job.job_id
    && candidate.scope.config_revision === job.config_revision
    && candidate.scope.target_index >= 0
    && candidate.scope.target_index < job.targets.length
  ));
  return entry?.scope.target_index ?? 0;
}

export function workspaceHasReviewEdits(workspace: CompareWorkspace): boolean {
  return workspace.reviewRevision > 0;
}
