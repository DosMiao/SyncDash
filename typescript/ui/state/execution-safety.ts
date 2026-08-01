import type { CompareOwner } from '../../core/types/generated/CompareOwner';
import type { PreflightDto } from '../../core/types/generated/PreflightDto';
import type { SelectedRowDto } from '../../core/types/generated/SelectedRowDto';

export function reviewedSetKey(
  owner: CompareOwner,
  jobId: string,
  configRevision: string,
  targetIndex: number,
  selected: SelectedRowDto[],
): string {
  return JSON.stringify([
    owner.compare_id,
    owner.job_id,
    owner.config_revision,
    owner.target_index,
    jobId,
    configRevision,
    targetIndex,
    selected.map((row) => [row.index, row.flipped]),
  ]);
}

export function preflightAllowsApply(
  preflight: PreflightDto | null,
  preflightError: string | null,
  acknowledged: boolean,
): boolean {
  return preflightError === null
    && preflight !== null
    && (preflight.ok || (preflight.acknowledgeable && acknowledged));
}

export interface AutoScanTicket {
  generation: number;
  ticketId: number;
  jobId: string;
  jobName: string;
  configRevision: string;
  targetIndex: number;
  autoApply: boolean;
}

export interface AutoScanSelection {
  jobId: string;
  configRevision: string;
  targetIndex: number;
}

export function ownsFreshAutoScanResult(
  enabled: boolean,
  active: AutoScanTicket | null,
  ticket: AutoScanTicket,
  owner: CompareOwner,
  selection: AutoScanSelection | null,
): boolean {
  return enabled
    && active !== null
    && selection !== null
    && active.generation === ticket.generation
    && active.ticketId === ticket.ticketId
    && active.jobId === ticket.jobId
    && active.configRevision === ticket.configRevision
    && active.targetIndex === ticket.targetIndex
    && ticket.jobId === owner.job_id
    && ticket.configRevision === owner.config_revision
    && ticket.targetIndex === owner.target_index
    && ticket.jobId === selection.jobId
    && ticket.configRevision === selection.configRevision
    && ticket.targetIndex === selection.targetIndex;
}

export type RootEditKeyAction = 'commit' | 'revert' | null;

export function rootEditKeyAction(key: string): RootEditKeyAction {
  if (key === 'Enter') return 'commit';
  if (key === 'Escape') return 'revert';
  return null;
}
