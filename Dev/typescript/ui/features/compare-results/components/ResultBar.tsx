import { useId, useMemo, useRef } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';
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
import { formatCount } from '#core/shared/format.ts';
import { RESULT_TYPE_DEFINITIONS, RESULT_TYPES, SORT_LABEL } from '#core/domain/compare/plan.ts';
import { ROOT_FOLDER_PATH, ROOT_LEVEL_LABEL } from '#core/domain/compare/folders.ts';
import type { PlanDto, ResultType, Sort } from '#core/domain/compare/plan.ts';
import type { CompareResultView } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import { useInteractionLayer } from '#ui/shared/interaction/useInteractionLayer.tsx';

/**
 * The three stages of the run decision, plus the constraints that produced them.
 *
 * `foundCount` is the complete difference result. `inScopeCount` is what remains
 * after search and every scope filter. `selectedCount` is the checked, executable
 * subset of that scope. Keeping all three explicit prevents a folded tree from
 * being mistaken for an execution filter.
 */
interface RunScopeSummary {
  foundCount: number;
  inScopeCount: number;
  selectedCount: number;
  folderScope: string | null;
  selectedResultTypes: readonly ResultType[];
  advancedFilterCount: number;
}

interface ResultBarProps {
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

/// One active scope constraint, shown as the control that clears it.
function ScopeBadge(props: {
  kind: 'folder' | 'result-types' | 'search' | 'filters';
  label: ReactNode;
  value: ReactNode;
  clearTitle: string;
  clearAriaLabel: string;
  onClear: () => void;
}) {
  const { kind, label, value, clearTitle, clearAriaLabel, onClear } = props;
  return (
    <button
      type="button"
      className={`result-bar-scope-badge result-bar-scope-${kind}`}
      title={clearTitle}
      aria-label={clearAriaLabel}
      onClick={onClear}
    >
      <span className="result-bar-scope-badge-label">{label}</span>
      {value}
      <X size={11} aria-hidden="true" />
    </button>
  );
}

interface SegmentedOption {
  key: string;
  label: ReactNode;
  active: boolean;
  onSelect: () => void;
}

/// A labelled group of mutually exclusive view options. Selecting the active option is a no-op, so
/// each option owns that guard rather than the group.
function SegmentedControl(props: {
  labelId: string;
  label: ReactNode;
  options: SegmentedOption[];
}) {
  const { labelId, label, options } = props;
  return (
    <div className="segmented-control" role="group" aria-labelledby={labelId}>
      <span id={labelId} className="segmented-label">{label}</span>
      {options.map((option) => (
        <button
          key={option.key}
          type="button"
          className={'segmented-option' + (option.active ? ' is-active' : '')}
          aria-pressed={option.active}
          onClick={option.onSelect}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
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
            <span className="result-bar-tab-count">{formatCount(plan.ops.length)}</span>
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
            <span className="result-bar-tab-count">{formatCount(plan.identical_count)}</span>
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
                <span className="control-count" aria-label={`${formatCount(scope.advancedFilterCount)} active`}>
                  {formatCount(scope.advancedFilterCount)}
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

            <SegmentedControl
              labelId={layoutLabelId}
              label="Layout"
              options={[
                {
                  key: 'tree',
                  label: <><FolderTree size={13} aria-hidden="true" />Tree</>,
                  active: grouped,
                  onSelect: () => { if (!grouped) onToggleGroup(); },
                },
                {
                  key: 'list',
                  label: <><List size={13} aria-hidden="true" />List</>,
                  active: !grouped,
                  onSelect: () => { if (grouped) onToggleGroup(); },
                },
              ]}
            />

            <SegmentedControl
              labelId={pathLabelId}
              label={<><Route size={13} aria-hidden="true" />Paths</>}
              options={[
                {
                  key: 'relative',
                  label: 'Relative',
                  active: pathMode === 'relative',
                  onSelect: () => { if (pathMode !== 'relative') onTogglePathMode(); },
                },
                {
                  key: 'full',
                  label: 'Full',
                  active: pathMode === 'full',
                  onSelect: () => { if (pathMode !== 'full') onTogglePathMode(); },
                },
              ]}
            />
          </div>

          <div className="result-bar-scope" aria-label="Run Scope Summary" aria-busy={scopeCalculationPending}>
            <div className="result-bar-scope-description">
              <span className="result-bar-scope-title">Run Scope</span>
              <div className="result-bar-scope-badges" aria-label="Active Scope Constraints">
                {!hasScopedResult && <span className="result-bar-scope-badge is-neutral">Any Difference</span>}
                {hasFolder && (
                  <ScopeBadge
                    kind="folder"
                    label="Folder"
                    value={scope.folderScope === ROOT_FOLDER_PATH ? ROOT_LEVEL_LABEL : scope.folderScope}
                    clearTitle="Clear folder scope"
                    clearAriaLabel={`Clear folder scope: ${scope.folderScope === ROOT_FOLDER_PATH ? ROOT_LEVEL_LABEL : scope.folderScope}`}
                    onClear={onClearFolderScope}
                  />
                )}
                {hasResultTypes && (
                  <ScopeBadge
                    kind="result-types"
                    label="Result Types"
                    value={activeResultTypeLabels.join(', ')}
                    clearTitle="Clear result-type scope"
                    clearAriaLabel={`Clear result-type scope: ${activeResultTypeLabels.join(', ')}`}
                    onClear={onClearSelectedResultTypes}
                  />
                )}
                {hasSearch && (
                  <ScopeBadge
                    kind="search"
                    label={searchPending ? 'Search Applying' : 'Search'}
                    value={`“${searchDraft.trim()}”`}
                    clearTitle="Clear search"
                    clearAriaLabel={`Clear search: ${searchDraft.trim()}`}
                    onClear={clearSearch}
                  />
                )}
                {hasAdvancedFilters && (
                  <ScopeBadge
                    kind="filters"
                    label="Advanced Filters"
                    value={formatCount(scope.advancedFilterCount)}
                    clearTitle="Clear advanced filters"
                    clearAriaLabel={`Clear ${formatCount(scope.advancedFilterCount)} active advanced filters`}
                    onClear={onClearAdvancedFilters}
                  />
                )}
              </div>
            </div>

            <dl className="result-bar-scope-counts" aria-label="Scope Counts" aria-live="polite">
              <div className="result-bar-scope-count">
                <dt>Found</dt>
                <dd>{formatCount(scope.foundCount)}</dd>
              </div>
              <div className="result-bar-scope-count">
                <dt>In Scope</dt>
                <dd>{formatCount(scope.inScopeCount)}</dd>
              </div>
              <div className="result-bar-scope-count result-bar-scope-count-selected">
                <dt>Selected</dt>
                <dd>{formatCount(scope.selectedCount)}</dd>
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
