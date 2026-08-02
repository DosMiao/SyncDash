import { useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';
import { ChevronDown, ChevronRight, PanelLeftClose, PanelLeftOpen, X } from 'lucide-react';
import { humanSize } from '#core/shared/format.ts';
import { ROOT_FOLDER_PATH, ROOT_LEVEL_LABEL } from '#core/domain/compare/folders.ts';
import { RESULT_TYPE_DEFINITIONS, RESULT_TYPES } from '#core/domain/compare/plan.ts';
import type { PlanDto, ResultType } from '#core/domain/compare/plan.ts';
import {
  buildRunScopeModel,
  flattenExpandedFolders,
} from '#core/domain/compare/runScope.ts';
import type { RunScopeFolderRow } from '#core/domain/compare/runScope.ts';
import { RESULT_TYPE_ICON } from '#ui/shared/icons/compareIcons.tsx';

export interface RunScopePanelProps {
  plan: PlanDto;
  reversedRows: boolean[];
  selectedResultTypes: Set<ResultType>;
  onSelectedResultTypesChange: (next: Set<ResultType>) => void;
  folderScope: string | null;
  onFolderScopeChange: (next: string | null) => void;
  collapsed: boolean;
  expandedFolders: Set<string>;
  onToggleCollapsed: () => void;
  onToggleExpandedFolder: (path: string) => void;
  onClearRunScope: () => void;
}

const ACTION_RESULT_TYPES = RESULT_TYPES.filter((type) => RESULT_TYPE_DEFINITIONS[type].group === 'action');
const REPORT_RESULT_TYPES = RESULT_TYPES.filter((type) => RESULT_TYPE_DEFINITIONS[type].group === 'report');
const COUNT_FORMAT = new Intl.NumberFormat('en-US');

function resultCountLabel(count: number): string {
  return `${COUNT_FORMAT.format(count)} ${count === 1 ? 'difference' : 'differences'}`;
}

function ResultTypeFacet(props: {
  resultType: ResultType;
  count: number;
  selected: boolean;
  onToggle: (resultType: ResultType) => void;
}) {
  const { resultType, count, selected, onToggle } = props;
  const definition = RESULT_TYPE_DEFINITIONS[resultType];
  const disabled = count === 0 && !selected;
  return (
    <button
      type="button"
      className={[
        'run-scope-facet',
        `result-type-${resultType}`,
        selected && 'on',
        count === 0 && 'zero',
      ].filter(Boolean).join(' ')}
      aria-pressed={selected}
      aria-label={`${definition.pluralLabel}: ${resultCountLabel(count)}. ${selected ? 'Remove' : 'Add'} this run-scope filter.`}
      disabled={disabled}
      onClick={() => onToggle(resultType)}
    >
      <span className="run-scope-facet-label">{RESULT_TYPE_ICON[resultType]}{definition.pluralLabel}</span>
      <span className="run-scope-facet-count">{COUNT_FORMAT.format(count)}</span>
    </button>
  );
}

function RunScopeFolderLayout(props: {
  children: ReactNode;
  depth: number;
  percent: number;
  selected: boolean;
}) {
  const { children, depth, percent, selected } = props;
  const folderRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const folder = folderRef.current;
    if (!folder) return;
    folder.style.setProperty('--run-scope-depth', String(depth));
    folder.style.setProperty('--run-scope-share', `${Math.min(100, percent)}%`);
    return () => {
      folder.style.removeProperty('--run-scope-depth');
      folder.style.removeProperty('--run-scope-share');
    };
  }, [depth, percent]);
  return (
    <div
      ref={folderRef}
      className={'run-scope-folder' + (selected ? ' on' : '')}
      role="none"
    >
      {children}
    </div>
  );
}

export function RunScopePanel(props: RunScopePanelProps) {
  const {
    plan,
    reversedRows,
    selectedResultTypes,
    onSelectedResultTypesChange,
    folderScope,
    onFolderScopeChange,
    collapsed,
    expandedFolders,
    onToggleCollapsed,
    onToggleExpandedFolder,
    onClearRunScope,
  } = props;

  const model = useMemo(() => buildRunScopeModel(plan, reversedRows), [plan, reversedRows]);
  const folderRows = useMemo(
    () => flattenExpandedFolders(model.folders, expandedFolders),
    [expandedFolders, model.folders],
  );
  const [focusedFolder, setFocusedFolder] = useState<string | null>(null);
  const folderRefs = useRef(new Map<string, HTMLElement>());
  const tabStopFolder = folderRows.some(({ node }) => node.path === focusedFolder)
    ? focusedFolder
    : folderRows.find(({ node }) => node.path === folderScope)?.node.path ?? folderRows[0]?.node.path ?? null;
  const total = plan.ops.length;
  const idPrefix = useId();
  const actionsTitleId = `${idPrefix}-actions`;
  const reportsTitleId = `${idPrefix}-reports`;
  const foldersTitleId = `${idPrefix}-folders`;

  const selectAnyResultType = () => {
    if (selectedResultTypes.size > 0) onSelectedResultTypesChange(new Set());
  };

  const toggleResultType = (resultType: ResultType) => {
    const next = new Set(selectedResultTypes);
    if (next.has(resultType)) next.delete(resultType); else next.add(resultType);
    // Selecting every result type has the same membership as no constraint. Canonicalizing that
    // state keeps the summary and Any Result Type control unambiguous.
    if (next.size === RESULT_TYPES.length) next.clear();
    onSelectedResultTypesChange(next);
  };

  const focusFolderAt = (index: number) => {
    const row = folderRows[Math.max(0, Math.min(folderRows.length - 1, index))];
    if (!row) return;
    setFocusedFolder(row.node.path);
    folderRefs.current.get(row.node.path)?.focus();
  };

  const onFolderKeyDown = (
    event: ReactKeyboardEvent<HTMLElement>,
    index: number,
    row: RunScopeFolderRow,
  ) => {
    const { node, depth } = row;
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      focusFolderAt(index + 1);
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      focusFolderAt(index - 1);
    } else if (event.key === 'Home') {
      event.preventDefault();
      focusFolderAt(0);
    } else if (event.key === 'End') {
      event.preventDefault();
      focusFolderAt(folderRows.length - 1);
    } else if (event.key === 'ArrowRight' && node.children.length > 0) {
      event.preventDefault();
      if (!expandedFolders.has(node.path)) onToggleExpandedFolder(node.path);
      else if (folderRows[index + 1]?.depth === depth + 1) focusFolderAt(index + 1);
    } else if (event.key === 'ArrowLeft') {
      event.preventDefault();
      if (node.children.length > 0 && expandedFolders.has(node.path)) {
        onToggleExpandedFolder(node.path);
        return;
      }
      for (let parent = index - 1; parent >= 0; parent--) {
        if (folderRows[parent].depth < depth) {
          focusFolderAt(parent);
          break;
        }
      }
    } else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      onFolderScopeChange(folderScope === node.path ? null : node.path);
    }
  };

  const facets = (resultTypes: readonly ResultType[]) => resultTypes.map((resultType) => (
    <ResultTypeFacet
      key={resultType}
      resultType={resultType}
      count={model.countByResultType[resultType]}
      selected={selectedResultTypes.has(resultType)}
      onToggle={toggleResultType}
    />
  ));

  return (
    <aside className={'run-scope-panel' + (collapsed ? ' collapsed' : '')} aria-label="Run Scope">
      <button
        type="button"
        className="icon-btn run-scope-toggle"
        title={collapsed ? 'Expand Run Scope' : 'Collapse Run Scope'}
        aria-label={collapsed ? 'Expand Run Scope' : 'Collapse Run Scope'}
        aria-expanded={!collapsed}
        onClick={onToggleCollapsed}
      >{collapsed ? <PanelLeftOpen size={15} /> : <PanelLeftClose size={15} />}</button>

      <div className="run-scope-body">
        <header className="run-scope-head">
          <span>Run Scope</span>
          <button type="button" className="run-scope-clear-all" onClick={onClearRunScope}>
            <X size={12} /> Clear Scope
          </button>
        </header>

        <button
          type="button"
          className={'run-scope-any' + (selectedResultTypes.size === 0 ? ' on' : '')}
          aria-pressed={selectedResultTypes.size === 0}
          onClick={selectAnyResultType}
        >
          <span>Any Result Type</span>
          <span>{COUNT_FORMAT.format(total)}</span>
        </button>

        <section className="run-scope-section" aria-labelledby={actionsTitleId}>
          <h2 id={actionsTitleId} className="run-scope-section-title">Planned Actions</h2>
          <div className="run-scope-facets" role="group" aria-label="Planned Action Filters">
            {facets(ACTION_RESULT_TYPES)}
          </div>
        </section>

        <section className="run-scope-section" aria-labelledby={reportsTitleId}>
          <h2 id={reportsTitleId} className="run-scope-section-title">Reports</h2>
          <div className="run-scope-facets" role="group" aria-label="Report Filters">
            {facets(REPORT_RESULT_TYPES)}
          </div>
        </section>

        <section className="run-scope-section run-scope-folders" aria-labelledby={foldersTitleId}>
          <div className="run-scope-section-head">
            <h2 id={foldersTitleId} className="run-scope-section-title">Folders</h2>
            <button
              type="button"
              className="run-scope-clear-folder"
              disabled={folderScope === null}
              onClick={() => onFolderScopeChange(null)}
            >Clear Folder Scope</button>
          </div>
          <div className={'run-scope-folder-current' + (folderScope !== null ? ' on' : '')} aria-live="polite">
            <span>{folderScope === null ? 'Any Folder' : 'Folder Scope'}</span>
            {folderScope !== null && (
              <strong title={folderScope === ROOT_FOLDER_PATH ? ROOT_LEVEL_LABEL : folderScope}>
                {folderScope === ROOT_FOLDER_PATH ? ROOT_LEVEL_LABEL : folderScope}
              </strong>
            )}
          </div>

          {folderRows.length > 0 ? (
            <div className="run-scope-folder-list" role="tree" aria-label="Folder Scopes">
              {folderRows.map((row, index) => {
                const { node, depth } = row;
                const selected = folderScope === node.path;
                const percent = (total > 0 ? node.resultCount / total : 0) * 100;
                const metrics = [
                  node.transferBytes > 0 ? `Transfer: ${humanSize(node.transferBytes)}` : '',
                  node.deletionBytes > 0 ? `Deletion: ${humanSize(node.deletionBytes)}` : '',
                ].filter(Boolean);
                const metricText = metrics.length > 0 ? metrics.join(' · ') : 'No transfer or deletion bytes';
                const pathLabel = node.path === ROOT_FOLDER_PATH ? ROOT_LEVEL_LABEL : node.path;
                return (
                  <RunScopeFolderLayout
                    key={node.path}
                    depth={depth}
                    percent={percent}
                    selected={selected}
                  >
                    <div
                      ref={(element) => {
                        if (element) folderRefs.current.set(node.path, element);
                        else folderRefs.current.delete(node.path);
                      }}
                      role="treeitem"
                      className="run-scope-folder-select"
                      tabIndex={tabStopFolder === node.path ? 0 : -1}
                      aria-level={depth + 1}
                      aria-expanded={node.children.length > 0 ? expandedFolders.has(node.path) : undefined}
                      aria-selected={selected}
                      aria-label={`${selected ? 'Clear' : 'Set'} folder scope ${pathLabel}: ${resultCountLabel(node.resultCount)}, ${percent.toFixed(1)} percent of all differences. ${metricText}.`}
                      onFocus={() => setFocusedFolder(node.path)}
                      onKeyDown={(event) => onFolderKeyDown(event, index, row)}
                      onClick={() => onFolderScopeChange(selected ? null : node.path)}
                    >
                      {node.children.length > 0 && (
                        <span
                          className="run-scope-folder-expand"
                          title={expandedFolders.has(node.path) ? `Collapse ${pathLabel}` : `Expand ${pathLabel}`}
                          aria-hidden="true"
                          onMouseDown={(event) => event.preventDefault()}
                          onClick={(event) => {
                            event.stopPropagation();
                            onToggleExpandedFolder(node.path);
                            folderRefs.current.get(node.path)?.focus();
                          }}
                        >
                          {expandedFolders.has(node.path)
                            ? <ChevronDown size={13} aria-hidden="true" />
                            : <ChevronRight size={13} aria-hidden="true" />}
                        </span>
                      )}
                      <span className="run-scope-folder-main">
                        <span className="run-scope-folder-chevron" aria-hidden="true" />
                        <span className="run-scope-folder-name" title={pathLabel}>{node.label}</span>
                        <span className="run-scope-folder-count">{resultCountLabel(node.resultCount)}</span>
                      </span>
                      <span className="run-scope-folder-share">
                        <span>Share of all differences: {percent.toFixed(1)}%</span>
                        {metrics.length > 0 && <span className="run-scope-folder-bytes">{metricText}</span>}
                      </span>
                      <span className="run-scope-folder-bar" aria-hidden="true">
                        <span />
                      </span>
                    </div>
                  </RunScopeFolderLayout>
                );
              })}
            </div>
          ) : (
            <p className="run-scope-empty">No Differences to Scope.</p>
          )}
        </section>
      </div>
    </aside>
  );
}
