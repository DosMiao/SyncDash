import { useId, useMemo, useRef } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent } from 'react';
import {
  ArrowUpDown,
  ChevronsDownUp,
  ChevronsUpDown,
  Download,
  Equal,
  FolderTree,
  List,
  ListFilter,
  Route,
  X,
} from 'lucide-react';
import { RESULT_TYPE_DEFINITIONS, RESULT_TYPES, SORT_LABEL } from '../../core/plan';
import { ROOT_FOLDER_PATH, ROOT_LEVEL_LABEL } from '../../core/folders';
import type { PlanDto, ResultType, Sort } from '../../core/plan';
import type { CompareResultView } from '../state/compareWorkspaceModel';
import { useInteractionLayer } from '../hooks/useInteractionLayer';

/**
 * The three stages of the run decision, plus the constraints that produced them.
 *
 * `foundCount` is the complete difference result. `inScopeCount` is what remains
 * after search and every scope filter. `selectedCount` is the checked, executable
 * subset of that scope. Keeping all three explicit prevents a folded tree from
 * being mistaken for an execution filter.
 */
export interface RunScopeSummary {
  foundCount: number;
  inScopeCount: number;
  selectedCount: number;
  folderScope: string | null;
  selectedResultTypes: readonly ResultType[];
  advancedFilterCount: number;
}

export interface ResultBarProps {
  plan: PlanDto;

  resultView: CompareResultView;
  onResultViewChange: (next: CompareResultView) => void;

  searchDraft: string;
  searchPending: boolean;
  scopeCalculationPending: boolean;
  scopeCalculationFailed: boolean;
  onSearchDraftChange: (next: string) => void;
  onClearSearch: () => void;

  scope: RunScopeSummary;
  onClearScope: () => void;
  onClearFolderScope: () => void;
  onClearSelectedResultTypes: () => void;
  onClearAdvancedFilters: () => void;

  advancedFiltersOpen: boolean;
  onToggleAdvancedFilters: (anchor: DOMRect) => void;
  exportPending: boolean;
  onExportCsv: () => void;

  grouped: boolean;
  sort: Sort | null;
  anyCollapsed: boolean;
  pathMode: 'relative' | 'full';
  onToggleFold: () => void;
  onToggleGroup: () => void;
  onClearSort: () => void;
  onTogglePathMode: () => void;

  resultPanelId: string;
  differencesTabId: string;
  identicalTabId: string;
}

function count(value: number): string {
  return value.toLocaleString();
}

export function ResultBar(props: ResultBarProps) {
  const {
    plan,
    resultView,
    onResultViewChange,
    searchDraft,
    searchPending,
    scopeCalculationPending,
    scopeCalculationFailed,
    onSearchDraftChange,
    onClearSearch,
    scope,
    onClearScope,
    onClearFolderScope,
    onClearSelectedResultTypes,
    onClearAdvancedFilters,
    advancedFiltersOpen,
    onToggleAdvancedFilters,
    exportPending,
    onExportCsv,
    grouped,
    sort,
    anyCollapsed,
    pathMode,
    onToggleFold,
    onToggleGroup,
    onClearSort,
    onTogglePathMode,
    resultPanelId,
    differencesTabId,
    identicalTabId,
  } = props;

  const searchRef = useRef<HTMLInputElement>(null);
  const differencesTabRef = useRef<HTMLButtonElement>(null);
  const identicalTabRef = useRef<HTMLButtonElement>(null);
  const tabLabelId = useId();
  const layoutLabelId = useId();
  const pathLabelId = useId();
  useInteractionLayer({
    kind: 'workspace',
    handlers: {
      find: resultView === 'differences' ? () => searchRef.current?.focus() : undefined,
    },
  });

  const activeResultTypeLabels = useMemo(() => {
    const active = new Set(scope.selectedResultTypes);
    return RESULT_TYPES
      .filter((resultType) => active.has(resultType))
      .map((resultType) => RESULT_TYPE_DEFINITIONS[resultType].label);
  }, [scope.selectedResultTypes]);

  const hasSearch = searchDraft.trim().length > 0;
  const hasFolder = scope.folderScope != null;
  const hasResultTypes = activeResultTypeLabels.length > 0;
  const hasAdvancedFilters = scope.advancedFilterCount > 0;
  const hasScopedResult = hasSearch || hasFolder || hasResultTypes || hasAdvancedFilters;

  const selectResultView = (next: CompareResultView) => {
    if (next !== resultView) onResultViewChange(next);
  };

  const onTabKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    let next: CompareResultView | null = null;
    if (event.key === 'Home') next = 'differences';
    if (event.key === 'End') next = 'identical';
    if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      next = resultView === 'differences' ? 'identical' : 'differences';
    }
    if (!next) return;
    event.preventDefault();
    selectResultView(next);
    (next === 'differences' ? differencesTabRef : identicalTabRef).current?.focus();
  };

  const clearSearch = () => {
    onClearSearch();
    searchRef.current?.focus();
  };

  return (
    <section className="result-bar" aria-label="Compare Results">
      <div className="result-bar-primary">
        <div className="result-bar-tabs" role="tablist" aria-labelledby={tabLabelId}>
          <span id={tabLabelId} className="sr-only">Result Set</span>
          <button
            ref={differencesTabRef}
            id={differencesTabId}
            type="button"
            role="tab"
            className={'result-bar-tab' + (resultView === 'differences' ? ' is-active' : '')}
            aria-selected={resultView === 'differences'}
            aria-controls={resultPanelId}
            tabIndex={resultView === 'differences' ? 0 : -1}
            onKeyDown={onTabKeyDown}
            onClick={() => selectResultView('differences')}
          >
            <span>Differences</span>
            <span className="result-bar-tab-count">{count(plan.ops.length)}</span>
          </button>
          <button
            ref={identicalTabRef}
            id={identicalTabId}
            type="button"
            role="tab"
            className={'result-bar-tab' + (resultView === 'identical' ? ' is-active' : '')}
            aria-selected={resultView === 'identical'}
            aria-controls={resultPanelId}
            tabIndex={resultView === 'identical' ? 0 : -1}
            onKeyDown={onTabKeyDown}
            onClick={() => selectResultView('identical')}
          >
            <Equal size={13} aria-hidden="true" />
            <span>Identical</span>
            <span className="result-bar-tab-count">{count(plan.identical_count)}</span>
          </button>
        </div>

        {resultView === 'differences' && (
          <div className="result-bar-primary-actions">
            <button
              type="button"
              className="btn result-bar-export"
              title={scopeCalculationFailed
                ? 'The run scope could not be calculated safely'
                : scopeCalculationPending
                  ? 'Wait for the run scope to finish calculating before exporting'
                  : exportPending
                    ? 'A CSV export is already in progress'
                  : scope.inScopeCount === 0
                  ? 'There are no in-scope differences to export'
                  : 'Export the current scoped and sorted differences as CSV'}
              disabled={exportPending || scopeCalculationPending || scopeCalculationFailed || scope.inScopeCount === 0}
              onClick={onExportCsv}
            >
              <Download size={13} aria-hidden="true" />
              {exportPending ? 'Exporting…' : 'Export CSV'}
            </button>
          </div>
        )}
      </div>

      {resultView === 'differences' && (
        <>
          <div className="result-bar-query">
            <div className="result-bar-search-wrap">
              <label className="sr-only" htmlFor={`${tabLabelId}-search`}>Search Differences</label>
              <input
                ref={searchRef}
                id={`${tabLabelId}-search`}
                className="result-bar-search mono"
                type="search"
                placeholder="Search paths and reasons…"
                title="Search by path, source path, or reason (Ctrl+F)"
                value={searchDraft}
                aria-busy={searchPending}
                onChange={(event) => onSearchDraftChange(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Escape' && searchDraft) {
                    event.preventDefault();
                    clearSearch();
                  }
                }}
              />
            </div>
            <button
              type="button"
              className={'btn advanced-filters-trigger' + (hasAdvancedFilters ? ' has-active-scope' : '')}
              aria-expanded={advancedFiltersOpen}
              aria-haspopup="dialog"
              onClick={(event) => {
                event.stopPropagation();
                onToggleAdvancedFilters(event.currentTarget.getBoundingClientRect());
              }}
            >
              <ListFilter size={13} aria-hidden="true" />
              Advanced Filters
              {hasAdvancedFilters && (
                <span className="control-count" aria-label={`${count(scope.advancedFilterCount)} active`}>
                  {count(scope.advancedFilterCount)}
                </span>
              )}
            </button>
          </div>

          <div className="result-bar-view" aria-label="View Options">
            {sort && (
              <button
                type="button"
                className="btn result-bar-clear-sort"
                title={`Sorted by ${SORT_LABEL[sort.key]}, ${sort.dir === 1 ? 'ascending' : 'descending'}. Click to clear.`}
                aria-label={`Clear sorting by ${SORT_LABEL[sort.key]}, ${sort.dir === 1 ? 'ascending' : 'descending'}`}
                onClick={onClearSort}
              >
                <ArrowUpDown size={13} aria-hidden="true" />
                <span>Sort: {SORT_LABEL[sort.key]}</span>
                <span aria-hidden="true">{sort.dir === 1 ? '↑' : '↓'}</span>
                <X size={12} aria-hidden="true" />
              </button>
            )}

            {grouped && (
              <button
                type="button"
                className="btn result-bar-folder-fold"
                title={anyCollapsed ? 'Expand every folder in the tree' : 'Collapse every folder in the tree'}
                onClick={onToggleFold}
              >
                {anyCollapsed
                  ? <ChevronsUpDown size={13} aria-hidden="true" />
                  : <ChevronsDownUp size={13} aria-hidden="true" />}
                {anyCollapsed ? 'Expand Folders' : 'Collapse Folders'}
              </button>
            )}

            <div className="segmented-control" role="group" aria-labelledby={layoutLabelId}>
              <span id={layoutLabelId} className="segmented-label">Layout</span>
              <button
                type="button"
                className={'segmented-option' + (grouped ? ' is-active' : '')}
                aria-pressed={grouped}
                onClick={() => { if (!grouped) onToggleGroup(); }}
              >
                <FolderTree size={13} aria-hidden="true" />
                Tree
              </button>
              <button
                type="button"
                className={'segmented-option' + (!grouped ? ' is-active' : '')}
                aria-pressed={!grouped}
                onClick={() => { if (grouped) onToggleGroup(); }}
              >
                <List size={13} aria-hidden="true" />
                List
              </button>
            </div>

            <div className="segmented-control" role="group" aria-labelledby={pathLabelId}>
              <span id={pathLabelId} className="segmented-label">
                <Route size={13} aria-hidden="true" />
                Paths
              </span>
              <button
                type="button"
                className={'segmented-option' + (pathMode === 'relative' ? ' is-active' : '')}
                aria-pressed={pathMode === 'relative'}
                onClick={() => { if (pathMode !== 'relative') onTogglePathMode(); }}
              >
                Relative
              </button>
              <button
                type="button"
                className={'segmented-option' + (pathMode === 'full' ? ' is-active' : '')}
                aria-pressed={pathMode === 'full'}
                onClick={() => { if (pathMode !== 'full') onTogglePathMode(); }}
              >
                Full
              </button>
            </div>
          </div>

          <div className="result-bar-scope" aria-label="Run Scope Summary" aria-busy={scopeCalculationPending}>
            <div className="result-bar-scope-description">
              <span className="result-bar-scope-title">Run Scope</span>
              <div className="result-bar-scope-badges" aria-label="Active Scope Constraints">
                {!hasScopedResult && <span className="result-bar-scope-badge is-neutral">Any Difference</span>}
                {hasFolder && (
                  <button
                    type="button"
                    className="result-bar-scope-badge result-bar-scope-folder"
                    title="Clear folder scope"
                    aria-label={`Clear folder scope: ${scope.folderScope === ROOT_FOLDER_PATH ? ROOT_LEVEL_LABEL : scope.folderScope}`}
                    onClick={onClearFolderScope}
                  >
                    <span className="result-bar-scope-badge-label">Folder</span>
                    {scope.folderScope === ROOT_FOLDER_PATH ? ROOT_LEVEL_LABEL : scope.folderScope}
                    <X size={11} aria-hidden="true" />
                  </button>
                )}
                {hasResultTypes && (
                  <button
                    type="button"
                    className="result-bar-scope-badge result-bar-scope-result-types"
                    title="Clear result-type scope"
                    aria-label={`Clear result-type scope: ${activeResultTypeLabels.join(', ')}`}
                    onClick={onClearSelectedResultTypes}
                  >
                    <span className="result-bar-scope-badge-label">Result Types</span>
                    {activeResultTypeLabels.join(', ')}
                    <X size={11} aria-hidden="true" />
                  </button>
                )}
                {hasSearch && (
                  <button
                    type="button"
                    className="result-bar-scope-badge result-bar-scope-search"
                    title="Clear search"
                    aria-label={`Clear search: ${searchDraft.trim()}`}
                    onClick={clearSearch}
                  >
                    <span className="result-bar-scope-badge-label">{searchPending ? 'Search Applying' : 'Search'}</span>
                    “{searchDraft.trim()}”
                    <X size={11} aria-hidden="true" />
                  </button>
                )}
                {hasAdvancedFilters && (
                  <button
                    type="button"
                    className="result-bar-scope-badge result-bar-scope-filters"
                    title="Clear advanced filters"
                    aria-label={`Clear ${count(scope.advancedFilterCount)} active advanced filters`}
                    onClick={onClearAdvancedFilters}
                  >
                    <span className="result-bar-scope-badge-label">Advanced Filters</span>
                    {count(scope.advancedFilterCount)}
                    <X size={11} aria-hidden="true" />
                  </button>
                )}
              </div>
            </div>

            <dl className="result-bar-scope-counts" aria-label="Scope Counts" aria-live="polite">
              <div className="result-bar-scope-count">
                <dt>Found</dt>
                <dd>{count(scope.foundCount)}</dd>
              </div>
              <div className="result-bar-scope-count">
                <dt>In Scope</dt>
                <dd>{count(scope.inScopeCount)}</dd>
              </div>
              <div className="result-bar-scope-count result-bar-scope-count-selected">
                <dt>Selected</dt>
                <dd>{count(scope.selectedCount)}</dd>
              </div>
            </dl>

            <button
              type="button"
              className="btn result-bar-clear-scope"
              title="Clear folder, result type, search, and advanced filters"
              disabled={!hasScopedResult}
              onClick={onClearScope}
            >
              <X size={13} aria-hidden="true" />
              Clear Scope
            </button>
          </div>

        </>
      )}
    </section>
  );
}
