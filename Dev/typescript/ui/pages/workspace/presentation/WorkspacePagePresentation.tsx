import type { ComponentProps, ReactNode } from 'react';
import { ConfirmSheet } from '#ui/features/apply-review/components/ConfirmSheet.tsx';
import { AdvancedFiltersPopover } from '#ui/features/compare-filters/components/AdvancedFiltersPopover.tsx';
import { CompareReviewSheet } from '#ui/features/operation-review/components/OperationReviewSheet.tsx';
import { ContextMenu } from '#ui/shared/components/menu/ContextMenu.tsx';
import { MenuDivider, MenuItem } from '#ui/shared/components/menu/Menu.tsx';
import { ConfirmDialog } from '#ui/shared/components/overlays/ConfirmDialog.tsx';
import type { ContextMenuState } from '../model/workspacePageModel.ts';
import { WorkspaceMainView, type WorkspaceMainViewProps } from './WorkspaceMainView.tsx';

export interface WorkspacePagePresentationProps {
  main: WorkspaceMainViewProps;
  contextMenu: { state: ContextMenuState; onClose: () => void } | null;
  advancedFilters: {
    key: string;
    props: ComponentProps<typeof AdvancedFiltersPopover>;
  } | null;
  jobEditor: ReactNode;
  settings: ReactNode;
  candidateDialog: ComponentProps<typeof ConfirmDialog> | null;
  rootSwapDialog: ComponentProps<typeof ConfirmDialog> | null;
  applyReview: ComponentProps<typeof ConfirmSheet> | null;
  compareReview: ComponentProps<typeof CompareReviewSheet> | null;
}

export function WorkspacePagePresentation({
  main,
  contextMenu,
  advancedFilters,
  jobEditor,
  settings,
  candidateDialog,
  rootSwapDialog,
  applyReview,
  compareReview,
}: WorkspacePagePresentationProps) {
  return (
    <>
      <WorkspaceMainView {...main} />
      {contextMenu && (
        <ContextMenu at={contextMenu.state} onClose={contextMenu.onClose}>
          {contextMenu.state.entries.map((entry, index) => (entry.separator
            ? <MenuDivider key={index} />
            : <MenuItem key={index} disabled={entry.disabled} danger={entry.danger} onClick={entry.run}>{entry.label}</MenuItem>
          ))}
        </ContextMenu>
      )}
      {advancedFilters && <AdvancedFiltersPopover key={advancedFilters.key} {...advancedFilters.props} />}
      {jobEditor}
      {settings}
      {candidateDialog && <ConfirmDialog {...candidateDialog} />}
      {rootSwapDialog && <ConfirmDialog {...rootSwapDialog} />}
      {applyReview && <ConfirmSheet {...applyReview} />}
      {compareReview && <CompareReviewSheet {...compareReview} />}
    </>
  );
}
