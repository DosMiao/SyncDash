import type { ComponentProps } from 'react';
import { CircleCheck, FolderSearch } from 'lucide-react';
import type { CompareResultView } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';
import { ComparePanel } from '#ui/features/compare-run/components/ComparePanel.tsx';
import { IdenticalResultsPanel } from '#ui/features/compare-results/components/IdenticalResultsPanel.tsx';
import { PlanTable } from '#ui/features/compare-results/components/PlanTable.tsx';
import { RunScopePanel } from '#ui/features/compare-results/components/RunScopePanel.tsx';
import { ScanFaultBanner } from '#ui/features/compare-results/components/ScanFaultBanner.tsx';
import { Placeholder } from '#ui/shared/components/feedback/Placeholder.tsx';

interface WorkspaceResultsSectionProps {
  plan: PlanDto | null;
  selectedJobName: string | null;
  resultView: CompareResultView;
  hasDifferences: boolean;
  compareActive: boolean;
  resultPanelId: string;
  differencesTabId: string;
  identicalTabId: string;
  setResultViewportElement: (element: HTMLDivElement | null) => void;
  runScopeProps: ComponentProps<typeof RunScopePanel> | null;
  comparePanelProps: ComponentProps<typeof ComparePanel>;
  identicalResultsProps: ComponentProps<typeof IdenticalResultsPanel> | null;
  planTableProps: ComponentProps<typeof PlanTable> | null;
}

export function WorkspaceResultsSection({
  plan,
  selectedJobName,
  resultView,
  hasDifferences,
  compareActive,
  resultPanelId,
  differencesTabId,
  identicalTabId,
  setResultViewportElement,
  runScopeProps,
  comparePanelProps,
  identicalResultsProps,
  planTableProps,
}: WorkspaceResultsSectionProps) {
  return (
    <>
      {plan && <ScanFaultBanner header={plan.header} />}
      <div className="results-layout">
        {runScopeProps && <RunScopePanel {...runScopeProps} />}
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
            <ComparePanel {...comparePanelProps} />
          ) : resultView === 'identical' && identicalResultsProps ? (
            <IdenticalResultsPanel {...identicalResultsProps} />
          ) : hasDifferences && planTableProps ? (
            <PlanTable {...planTableProps} />
          ) : plan ? (
            <Placeholder
              icon={<CircleCheck size={26} className="icon-ok" />}
              title="No Differences in the Compared Scope"
              description="No synchronization actions are planned. Identical lists matching files; the status bar preserves excluded and unread counts."
            />
          ) : (
            <Placeholder
              icon={<FolderSearch size={26} />}
              title={selectedJobName ? `Ready — ${selectedJobName}` : 'No Job Selected'}
              description={selectedJobName
                ? 'Press Compare (F5 or Ctrl+R) to walk both roots and build a plan.'
                : 'Pick a job on the left, then press Compare (F5 or Ctrl+R).'}
            />
          )}
        </div>
      </div>
    </>
  );
}
