import type { Dispatch } from 'react';
import type { CompareForgetAvailability } from '#core/application/compare-workspace/compareWorkspaceForget.ts';
import type { CompareWorkspaceRepository } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import type { ApplyReviewTotals } from '#ui/features/apply-review/model/applyReviewTotals.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import type { JobEditorProps } from '#ui/features/jobs/controllers/JobEditorController.tsx';
import type { SettingsSheetProps } from '#ui/features/settings/controllers/SettingsSheetController.tsx';
import type { useApplyExecutionController } from '../../runtime/apply/useApplyExecutionController.ts';
import type { useCompareExecutionController } from '../../runtime/compare/useCompareExecutionController.ts';
import type { useForgetCompareResult } from '../../runtime/results/useForgetCompareResult.ts';
import type { useJobEditorLifecycle } from '../../runtime/jobs/useJobEditorLifecycle.ts';
import type { useJobExcludeMutation } from '../../runtime/jobs/useJobExcludeMutation.ts';
import type { useWorkspacePanels } from '../../runtime/interaction/useWorkspacePanels.ts';
import type { useWorkspaceReviewState } from '../../runtime/reviews/useWorkspaceReviewState.ts';
import type { useRootMutations } from '../../runtime/roots/useRootMutations.ts';
import type { useWorkspaceResultViewModel } from '../../view-model/useWorkspaceResultViewModel.ts';
import type { useWorkspaceSessionState } from '../../runtime/session/useWorkspaceSessionState.ts';
import type { WorkspacePagePresentationProps } from '../../presentation/WorkspacePagePresentation.tsx';

interface WorkspaceOverlayPresentationOptions {
  addExcludes: ReturnType<typeof useJobExcludeMutation>;
  apply: ReturnType<typeof useApplyExecutionController>;
  busy: boolean;
  compare: ReturnType<typeof useCompareExecutionController>;
  confirmTotals: ApplyReviewTotals | null;
  dispatchCompare: Dispatch<CompareWorkspaceAction>;
  editorLifecycle: ReturnType<typeof useJobEditorLifecycle>;
  forget: ReturnType<typeof useForgetCompareResult>;
  forgetAvailability: CompareForgetAvailability;
  panels: ReturnType<typeof useWorkspacePanels>;
  repository: CompareWorkspaceRepository;
  resetSafetyUi: () => void;
  result: ReturnType<typeof useWorkspaceResultViewModel>;
  reviews: ReturnType<typeof useWorkspaceReviewState>;
  rootMutations: ReturnType<typeof useRootMutations>;
  session: ReturnType<typeof useWorkspaceSessionState>;
  statusApi: StatusApi;
}

type RenderedWorkspaceOverlays = Omit<WorkspacePagePresentationProps, 'main' | 'jobEditor' | 'settings'>;

export interface WorkspaceOverlayComposition {
  presentation: RenderedWorkspaceOverlays;
  jobEditor: JobEditorProps | null;
  settings: SettingsSheetProps | null;
}

/** Builds only overlay contracts; ownership and mutation fencing remain in feature runtimes. */
export function createWorkspaceOverlayPresentation({
  addExcludes,
  apply,
  busy,
  compare,
  confirmTotals,
  dispatchCompare,
  editorLifecycle,
  forget,
  forgetAvailability,
  panels,
  repository,
  resetSafetyUi,
  result,
  reviews,
  rootMutations,
  session,
  statusApi,
}: WorkspaceOverlayPresentationOptions): WorkspaceOverlayComposition {
  const { plan, appliedAdvancedFilter, inScopeIndices, selectedCompareWorkspace, workspaceController } = result;
  const { askSwap, candidateAdoption, editor, forgetRequest } = panels;

  const presentation: RenderedWorkspaceOverlays = {
    contextMenu: panels.contextMenu
      ? { state: panels.contextMenu, onClose: () => panels.setContextMenu(null) }
      : null,
    advancedFilters: panels.advancedFiltersAnchor && plan && selectedCompareWorkspace ? {
      key: selectedCompareWorkspace.key,
      props: {
        anchor: panels.advancedFiltersAnchor,
        appliedFilter: appliedAdvancedFilter,
        inScopeCount: inScopeIndices.length,
        differenceCount: plan.ops.length,
        onApplyFilter: workspaceController.applyAdvancedFilter,
        onWriteValidatedFilterMasksToJobExclude: (validatedFilter) => {
          void addExcludes(validatedFilter.masks, 'Written into the exclude list');
        },
        onDismiss: () => panels.setAdvancedFiltersAnchor(null),
      },
    } : null,
    candidateDialog: candidateAdoption ? {
      title: 'Review the newer AutoScan result?',
      message: 'The newer exact result will replace this active review workspace. Your current row decisions, filters, folds, and scroll position do not carry into different filesystem evidence.',
      action: {
        label: 'Review New Result',
        onConfirm: () => {
          const scope = repository.scopes.find((entry) => entry.key === candidateAdoption.scopeKey);
          if (scope?.candidate?.workspace.key !== candidateAdoption.resultKey) {
            statusApi.setMessage('The AutoScan candidate changed — review the newer candidate before adopting it', 'err');
            panels.setCandidateAdoption(null);
            return;
          }
          dispatchCompare({
            type: 'candidate_adopted',
            scopeKey: candidateAdoption.scopeKey,
            expectedResultKey: candidateAdoption.resultKey,
          });
          panels.setCandidateAdoption(null);
          resetSafetyUi();
        },
      },
      onCancel: () => panels.setCandidateAdoption(null),
    } : null,
    forgetDialog: forgetRequest ? {
      title: 'Permanently forget this Compare result?',
      message: 'This deletes the stored evidence for this exact result:\n\n'
        + '· Its plan, Identical snapshot, and this review workspace are discarded\n'
        + '· Neither root is touched — no file on either side is read, moved or removed\n'
        + '· Past run logs and anything already in the trash are left alone\n\n'
        + 'Forgetting cannot be undone. Only a new Compare can produce evidence for this job and target again.',
      action: {
        label: 'Forget this result',
        danger: true,
        disabled: forget.forgetPending || !forgetAvailability.available,
        onConfirm: () => { void forget.forgetResult(forgetRequest); },
      },
      onCancel: () => panels.setForgetRequest(null),
    } : null,
    rootSwapDialog: askSwap ? {
      title: `Swap the two roots of '${askSwap.owner.jobName}'?`,
      message: `source ← ${askSwap.values.target}\n`
        + `target ${askSwap.owner.targetIndex + 1} ← ${askSwap.values.source}\n\n`
        + (askSwap.mode === 'mirror'
          ? 'In mirror mode this reverses which side is authoritative: after the swap, the original target wins.\n\n'
          : '')
        + 'The job file is rewritten atomically. Existing Compare evidence remains viewable, but cannot be applied after the configuration changes. The status bar keeps an undo.',
      action: { label: 'Swap them', onConfirm: () => { void rootMutations.doSwap(askSwap); } },
      onCancel: () => panels.setAskSwap(null),
    } : null,
    applyReview: reviews.confirmOpen && session.selectedJob && confirmTotals ? {
      job: session.selectedJob,
      totals: confirmTotals,
      reviewState: reviews.applyReview,
      onCancel: reviews.resetConfirmation,
      onConfirm: () => { void apply.doSync(); },
      onReviewAgain: () => {
        reviews.resetConfirmation();
        void apply.openConfirm();
      },
    } : null,
    compareReview: reviews.compareReview.phase !== 'idle'
      && reviews.compareReview.phase !== 'authorized' ? {
        state: reviews.compareReview,
        onCancel: reviews.resetCompareReview,
        onReviewAgain: () => {
          reviews.resetCompareReview();
          void compare.doCompare();
        },
      } : null,
  };
  const jobEditor: JobEditorProps | null = editor ? {
    name: editor.name,
    focusGroup: editor.focusGroup,
    dropTargetKey: panels.dropTargetKey,
    scopeRef: panels.setEditorScope,
    apiRef: panels.editorApi,
    busy,
    onClose: () => panels.setEditor(null),
    onSaved: editorLifecycle.onSaved,
    onDeleted: editorLifecycle.onDeleted,
    onMutationConflict: editorLifecycle.onMutationConflict,
    onStatus: statusApi.setMessage,
  } : null;
  const settings: SettingsSheetProps | null = panels.settingsOpen ? {
    onClose: () => panels.setSettingsOpen(false),
    onSaved: (message, appearance) => {
      panels.setSettingsOpen(false);
      statusApi.setMessage(message, appearance);
      panels.setLogReload((revision) => revision + 1);
    },
    onStatus: statusApi.setMessage,
  } : null;
  return { presentation, jobEditor, settings };
}
