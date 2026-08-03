import { invoke } from '@tauri-apps/api/core';

import { compareAuthorizationArgs } from '#core/application/operations/operationProtocol.ts';
import type { RunEventEnvelope } from '#core/domain/runs/runEvents.ts';
import type { CompareFileSideDto } from '#core/types/generated/CompareFileSideDto.ts';
import type { CompareIdentity } from '#core/types/generated/CompareIdentity.ts';
import type { CompareResultForgetDto } from '#core/types/generated/CompareResultForgetDto.ts';
import type { CompareWorkspaceLookupDto } from '#core/types/generated/CompareWorkspaceLookupDto.ts';
import type { CompareWorkspaceSnapshotDto } from '#core/types/generated/CompareWorkspaceSnapshotDto.ts';
import type { CsvExportDto } from '#core/types/generated/CsvExportDto.ts';
import type { CsvRowPresentationDto } from '#core/types/generated/CsvRowPresentationDto.ts';
import type { IdenticalPage } from '#core/types/generated/IdenticalPage.ts';

export const compareJob = (authorizationToken: string) =>
  invoke<CompareWorkspaceSnapshotDto>('compare_job', compareAuthorizationArgs(authorizationToken));

export const reconcileCompareWorkspace = (compareIdentity: CompareIdentity) =>
  invoke<CompareWorkspaceLookupDto>('reconcile_compare_workspace', { compareIdentity });

export const restoreCompare = (jobId: string, targetIndex: number, expectedConfigRevision: string) =>
  invoke<CompareWorkspaceLookupDto>('restore_compare', { jobId, targetIndex, expectedConfigRevision });

export const forgetCompareResult = (compareIdentity: CompareIdentity) =>
  invoke<CompareResultForgetDto>('forget_compare_result', { compareIdentity });

export const cancelCompareRun = (runId: number) => invoke<boolean>('cancel_compare_run', { runId });
export const replayCompareEvents = (afterSequence = 0) =>
  invoke<RunEventEnvelope[]>('replay_compare_events', { afterSequence });

// Mask matching stays in Rust so UI previews and persisted job filters share one grammar.
export const maskMatch = (masks: string[], paths: string[]) =>
  invoke<boolean[]>('mask_match', { masks, paths });

export const listIdentical = (compareIdentity: CompareIdentity, query: string, offset: number, limit: number) =>
  invoke<IdenticalPage>('list_identical', { compareIdentity, query, offset, limit });

export const exportCompareCsv = (compareIdentity: CompareIdentity, rows: CsvRowPresentationDto[]) =>
  invoke<CsvExportDto>('export_compare_csv', { compareIdentity, rows });

export const revealCompareRow = (
  compareIdentity: CompareIdentity,
  index: number,
  side: CompareFileSideDto,
  directionReversed: boolean,
) => invoke<void>('reveal_compare_row', { compareIdentity, index, side, directionReversed });

export const revealCsvExport = (receiptId: string) =>
  invoke<void>('reveal_csv_export', { receiptId });
