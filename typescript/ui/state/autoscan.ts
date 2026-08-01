import type { AutoScanStatusDto } from '../../core/ipc';
import type { CompareOwner } from '../../core/types/generated/CompareOwner';

export const AUTOSCAN_TICKET_LEDGER_CAPACITY = 64;

export interface AutoScanTicket {
  generation: number;
  ticketId: number;
  jobId: string;
  jobName: string;
  configRevision: string;
  targetIndex: number;
  autoApply: boolean;
}

export type AutoScanStatusSource = 'event' | 'snapshot' | 'start' | 'completion' | 'stop';

function sameBinding(left: AutoScanStatusDto, right: AutoScanStatusDto): boolean {
  if (left.job_id === null || right.job_id === null) return !left.active && !right.active;
  return left.job_id === right.job_id
    && left.config_revision === right.config_revision
    && left.target_index === right.target_index;
}

function statusRank(status: AutoScanStatusDto): number {
  if (!status.active) return 4;
  if (status.pending_trigger || status.active_ticket !== null) return 2;
  if (status.latest_ticket_id > 0) return 3;
  return status.mode === 'starting' ? 0 : 1;
}

/**
 * Reconcile snapshots, events, and command responses without allowing an older worker generation
 * (or its initial `starting` snapshot) to resurrect or regress the monitor shown in the toolbar.
 * The never-armed DTO uses generation zero; stopped monitors retain their generation/cursor. The
 * zero-generation normalization is a defensive fallback, and a stale status read cannot invoke it.
 */
export function reconcileAutoScanStatus(
  current: AutoScanStatusDto | null,
  incoming: AutoScanStatusDto,
  source: AutoScanStatusSource,
  completedTicketId?: number,
): AutoScanStatusDto | null {
  if (!current) return incoming;

  let candidate = incoming;
  if (!incoming.active && incoming.generation === 0 && current.active) {
    if (source === 'snapshot' || source === 'start') return current;
    candidate = {
      ...incoming,
      generation: current.generation,
      latest_ticket_id: Math.max(current.latest_ticket_id, incoming.latest_ticket_id),
      job_id: current.job_id,
      job_name: current.job_name,
      config_revision: current.config_revision,
      target_index: current.target_index,
    };
  }

  if (candidate.generation < current.generation) return current;
  if (candidate.generation > current.generation) return candidate;
  if (!sameBinding(current, candidate)) return current;
  // A terminal tombstone for this generation cannot be resurrected by a delayed worker event.
  if (!current.active && candidate.active) return current;
  if (candidate.latest_ticket_id < current.latest_ticket_id) return current;
  if (candidate.latest_ticket_id > current.latest_ticket_id) return candidate;
  if (source === 'completion' && completedTicketId !== undefined) {
    const currentPending = current.pending_trigger?.ticket_id ?? current.active_ticket;
    if (currentPending !== null && currentPending !== completedTicketId) return current;
  }
  if (statusRank(candidate) < statusRank(current)) return current;
  return candidate;
}

export function monitorOwnsAutoScanResult(
  status: AutoScanStatusDto | null,
  active: AutoScanTicket | null,
  ticket: AutoScanTicket,
  owner: CompareOwner,
): boolean {
  return monitorOwnsAutoScanTicket(status, active, ticket)
    && owner.job_id === ticket.jobId
    && owner.config_revision === ticket.configRevision
    && owner.target_index === ticket.targetIndex;
}

export function monitorOwnsAutoScanTicket(
  status: AutoScanStatusDto | null,
  active: AutoScanTicket | null,
  ticket: AutoScanTicket,
): boolean {
  return statusCanOwnAutoScanTrigger(status, ticket)
    && active !== null
    && active.generation === ticket.generation
    && active.ticketId === ticket.ticketId
    && active.jobId === ticket.jobId
    && active.configRevision === ticket.configRevision
    && active.targetIndex === ticket.targetIndex;
}

export function statusCanOwnAutoScanTrigger(
  status: AutoScanStatusDto | null,
  ticket: AutoScanTicket,
): boolean {
  return status?.active === true
    && status.generation === ticket.generation
    && status.job_id === ticket.jobId
    && status.config_revision === ticket.configRevision
    && status.target_index === ticket.targetIndex
    && status.latest_ticket_id <= ticket.ticketId
    && (status.active_ticket === null || status.active_ticket === ticket.ticketId)
    && (status.pending_trigger === null || status.pending_trigger.ticket_id === ticket.ticketId);
}

export type AutoScanToggleAction = 'start' | 'stop' | 'unavailable';

export function autoScanToggleAction(status: AutoScanStatusDto | null, hasSelection: boolean): AutoScanToggleAction {
  if (status?.active) return 'stop';
  return hasSelection ? 'start' : 'unavailable';
}

export function autoScanButtonLabel(status: AutoScanStatusDto | null, pending: 'start' | 'stop' | null): string {
  if (pending === 'start') return 'Starting AutoScan…';
  if (pending === 'stop') return 'Stopping AutoScan…';
  if (!status?.active) return 'AutoScan';
  const target = status.target_index === null ? '' : ` · T${status.target_index + 1}`;
  return status.job_name ? `AutoScan · ${status.job_name}${target}` : `AutoScan · active${target}`;
}

type LedgerRecord<T> =
  | { stage: 'processing' }
  | { stage: 'completing'; outcome: T }
  | { stage: 'ready'; outcome: T }
  | { stage: 'completed' };

export type AutoScanTicketClaim<T> =
  | { kind: 'process' }
  | { kind: 'retry_completion'; outcome: T }
  | { kind: 'duplicate' }
  | { kind: 'capacity' };

function ticketKey(ticket: Pick<AutoScanTicket, 'generation' | 'ticketId'>): string {
  return `${ticket.generation}:${ticket.ticketId}`;
}

/**
 * Bounded, generation/ticket keyed at-most-once processing. A failed completion acknowledgement can
 * be retried from a recovered `pending_trigger` without rerunning Compare. Completed records are the
 * only records evicted; in-flight evidence is never silently forgotten.
 */
export class AutoScanTicketLedger<T> {
  readonly #capacity: number;
  readonly #records = new Map<string, LedgerRecord<T>>();

  constructor(capacity = AUTOSCAN_TICKET_LEDGER_CAPACITY) {
    if (!Number.isInteger(capacity) || capacity < 1) throw new Error('AutoScan ledger capacity must be positive');
    this.#capacity = capacity;
  }

  get size(): number { return this.#records.size; }

  claim(ticket: Pick<AutoScanTicket, 'generation' | 'ticketId'>): AutoScanTicketClaim<T> {
    const key = ticketKey(ticket);
    const existing = this.#records.get(key);
    if (existing?.stage === 'ready') {
      this.#records.set(key, { stage: 'completing', outcome: existing.outcome });
      return { kind: 'retry_completion', outcome: existing.outcome };
    }
    if (existing) return { kind: 'duplicate' };
    this.#trimCompleted();
    if (this.#records.size >= this.#capacity) return { kind: 'capacity' };
    this.#records.set(key, { stage: 'processing' });
    return { kind: 'process' };
  }

  prepareCompletion(ticket: Pick<AutoScanTicket, 'generation' | 'ticketId'>, outcome: T): boolean {
    const key = ticketKey(ticket);
    if (this.#records.get(key)?.stage !== 'processing') return false;
    this.#records.set(key, { stage: 'completing', outcome });
    return true;
  }

  completionFailed(ticket: Pick<AutoScanTicket, 'generation' | 'ticketId'>): void {
    const key = ticketKey(ticket);
    const record = this.#records.get(key);
    if (record?.stage === 'completing') this.#records.set(key, { stage: 'ready', outcome: record.outcome });
  }

  completed(ticket: Pick<AutoScanTicket, 'generation' | 'ticketId'>): void {
    const key = ticketKey(ticket);
    if (this.#records.has(key)) this.#records.set(key, { stage: 'completed' });
  }

  #trimCompleted(): void {
    while (this.#records.size >= this.#capacity) {
      const completed = [...this.#records].find(([, record]) => record.stage === 'completed');
      if (!completed) return;
      this.#records.delete(completed[0]);
    }
  }
}
