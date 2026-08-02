import { useCallback, useRef, useState } from 'react';
import type { MutableRefObject } from 'react';
import * as compareIpc from '#core/infrastructure/tauri/commands/compare.ts';
import type { CompareResultKey, CompareWorkspace } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { PlanLayout } from '#core/domain/compare/grouping.ts';
import type { PlanDto } from '#core/domain/compare/plan.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

interface CsvExportOptions {
  plan: PlanDto | null;
  workspace: CompareWorkspace | null;
  selectedWorkspaceKeyRef: MutableRefObject<CompareResultKey | null>;
  selectedJob: JobDto | null;
  resultView: 'differences' | 'identical';
  scopeCalculationFailed: boolean;
  scopeCalculationPending: boolean;
  layout: PlanLayout;
  reversedRows: readonly boolean[];
  includedRows: readonly boolean[];
  setStatus: StatusApi['setMessage'];
  offerStatusAction: StatusApi['offerAction'];
}

export function useCsvExport({
  plan,
  workspace,
  selectedWorkspaceKeyRef,
  selectedJob,
  resultView,
  scopeCalculationFailed,
  scopeCalculationPending,
  layout,
  reversedRows,
  includedRows,
  setStatus,
  offerStatusAction,
}: CsvExportOptions) {
  const exportInFlight = useRef<CompareResultKey | null>(null);
  const [pending, setPending] = useState(false);

  const exportCsv = useCallback(async () => {
    if (!plan || !workspace) { setStatus('Compare first', 'err'); return; }
    if (resultView !== 'differences') { setStatus('Switch to Differences before exporting', 'err'); return; }
    if (scopeCalculationFailed) { setStatus('The run scope could not be calculated safely', 'err'); return; }
    if (scopeCalculationPending) { setStatus('The run scope is still being calculated', 'err'); return; }
    if (exportInFlight.current !== null) return;

    const resultKey = workspace.key;
    const compareIdentity = workspace.identity;
    const rowPresentation = layout.displayOrder.map((index) => ({
      index,
      included: includedRows[index] === true,
      direction_reversed: reversedRows[index] === true,
    }));
    const scopeLabel = selectedJob
      ? `'${selectedJob.name}', target ${compareIdentity.target_index + 1}`
      : `job ${compareIdentity.job_id}, target ${compareIdentity.target_index + 1}`;

    exportInFlight.current = resultKey;
    setPending(true);
    try {
      const result = await compareIpc.exportCompareCsv(compareIdentity, rowPresentation);
      if (result.status === 'cancelled') return;
      const scopeSuffix = selectedWorkspaceKeyRef.current === resultKey ? '' : ` from ${scopeLabel}`;
      offerStatusAction(
        `Exported ${result.row_count} rows${scopeSuffix} to ${result.display_path}`,
        'Open containing folder',
        () => compareIpc.revealCsvExport(result.receipt_id),
      );
    } catch (error) {
      setStatus(`Export failed for ${scopeLabel}: ${error}`, 'err');
    } finally {
      if (exportInFlight.current === resultKey) {
        exportInFlight.current = null;
        setPending(false);
      }
    }
  }, [
    includedRows,
    layout.displayOrder,
    offerStatusAction,
    plan,
    resultView,
    reversedRows,
    scopeCalculationFailed,
    scopeCalculationPending,
    selectedJob,
    selectedWorkspaceKeyRef,
    setStatus,
    workspace,
  ]);

  return { exportCsv, exportPending: pending };
}
