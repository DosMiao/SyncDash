import { useCallback } from 'react';
import type { Dispatch, SetStateAction } from 'react';
import type {
  CompareResultView,
  CompareWorkspace,
  CompareWorkspacePreferences,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import type { AdvancedScopeFilter } from '#core/domain/compare/runScope.ts';
import { EMPTY_ADVANCED_SCOPE_FILTER } from '#core/domain/compare/runScope.ts';
import type { ResultType } from '#core/domain/compare/plan.ts';
import { EMPTY_PATH_SET, EMPTY_RESULT_TYPES } from '../../model/workspacePageModel.ts';

interface RunScopeActionsOptions {
  workspace: CompareWorkspace | null;
  preferences: CompareWorkspacePreferences;
  panelCollapsed: boolean;
  grouped: boolean;
  pathMode: CompareWorkspacePreferences['pathMode'];
  anyCollapsed: boolean;
  folderPathsInLayout: readonly string[];
  dispatch: Dispatch<CompareWorkspaceAction>;
  setPreferences: Dispatch<SetStateAction<CompareWorkspacePreferences>>;
  persistPreferences: (preferences: CompareWorkspacePreferences) => void;
  setAdvancedFiltersAnchor: Dispatch<SetStateAction<DOMRect | null>>;
  changeSearchDraft: (query: string) => void;
  applyAdvancedFilter: (filter: AdvancedScopeFilter) => void;
}

export function useRunScopeActions({
  workspace,
  preferences,
  panelCollapsed,
  grouped,
  pathMode,
  anyCollapsed,
  folderPathsInLayout,
  dispatch,
  setPreferences,
  persistPreferences,
  setAdvancedFiltersAnchor,
  changeSearchDraft,
  applyAdvancedFilter,
}: RunScopeActionsOptions) {
  const clearAdvancedFilters = useCallback(() => {
    applyAdvancedFilter({ ...EMPTY_ADVANCED_SCOPE_FILTER, masks: [] });
    setAdvancedFiltersAnchor(null);
  }, [applyAdvancedFilter, setAdvancedFiltersAnchor]);

  const clearRunScope = useCallback(() => {
    if (workspace) {
      changeSearchDraft('');
      dispatch({
        type: 'selected_result_types_changed',
        resultKey: workspace.key,
        resultTypes: EMPTY_RESULT_TYPES,
      });
      dispatch({ type: 'folder_scope_changed', resultKey: workspace.key, folderScope: null });
      applyAdvancedFilter({ ...EMPTY_ADVANCED_SCOPE_FILTER, masks: [] });
    }
    setAdvancedFiltersAnchor(null);
  }, [applyAdvancedFilter, changeSearchDraft, dispatch, setAdvancedFiltersAnchor, workspace]);

  const changeSelectedResultTypes = useCallback((resultTypes: ReadonlySet<ResultType>) => {
    if (!workspace) return;
    dispatch({ type: 'selected_result_types_changed', resultKey: workspace.key, resultTypes });
  }, [dispatch, workspace]);

  const changeFolderScope = useCallback((folderScope: string | null) => {
    if (!workspace) return;
    dispatch({ type: 'folder_scope_changed', resultKey: workspace.key, folderScope });
  }, [dispatch, workspace]);

  const togglePanel = useCallback(() => {
    if (!workspace) return;
    const collapsed = !panelCollapsed;
    dispatch({ type: 'scope_panel_collapsed_changed', resultKey: workspace.key, collapsed });
    const nextPreferences = { ...preferences, scopePanelCollapsed: collapsed };
    setPreferences(nextPreferences);
    persistPreferences(nextPreferences);
  }, [dispatch, panelCollapsed, persistPreferences, preferences, setPreferences, workspace]);

  const toggleExpandedFolder = useCallback((folderPath: string) => {
    if (!workspace) return;
    dispatch({ type: 'scope_folder_expansion_toggled', resultKey: workspace.key, folderPath });
  }, [dispatch, workspace]);

  const toggleAllFolds = useCallback(() => {
    if (!workspace) return;
    dispatch({
      type: 'difference_folds_replaced',
      resultKey: workspace.key,
      collapsedFolders: anyCollapsed ? EMPTY_PATH_SET : new Set(folderPathsInLayout),
    });
  }, [anyCollapsed, dispatch, folderPathsInLayout, workspace]);

  const toggleGrouping = useCallback(() => {
    const next = !grouped;
    if (workspace) dispatch({ type: 'difference_grouping_changed', resultKey: workspace.key, grouped: next });
    const nextPreferences = { ...preferences, grouped: next };
    setPreferences(nextPreferences);
    persistPreferences(nextPreferences);
  }, [dispatch, grouped, persistPreferences, preferences, setPreferences, workspace]);

  const clearSort = useCallback(() => {
    if (!workspace) return;
    dispatch({ type: 'difference_sort_changed', resultKey: workspace.key, sort: null });
  }, [dispatch, workspace]);

  const togglePathMode = useCallback(() => {
    const next: CompareWorkspacePreferences['pathMode'] = pathMode === 'relative' ? 'full' : 'relative';
    if (workspace) dispatch({ type: 'path_mode_changed', resultKey: workspace.key, pathMode: next });
    const nextPreferences = { ...preferences, pathMode: next };
    setPreferences(nextPreferences);
    persistPreferences(nextPreferences);
  }, [dispatch, pathMode, persistPreferences, preferences, setPreferences, workspace]);

  const changeResultView = useCallback((view: CompareResultView) => {
    if (workspace) dispatch({ type: 'result_view_changed', resultKey: workspace.key, view });
    if (view === 'identical') setAdvancedFiltersAnchor(null);
  }, [dispatch, setAdvancedFiltersAnchor, workspace]);

  return {
    changeFolderScope,
    changeResultView,
    changeSelectedResultTypes,
    clearAdvancedFilters,
    clearRunScope,
    clearSort,
    toggleAllFolds,
    toggleExpandedFolder,
    toggleGrouping,
    togglePanel,
    togglePathMode,
  };
}
