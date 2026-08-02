import type { AutoScanStatusDto } from '#core/types/generated/AutoScanStatusDto.ts';
import type { CompareOwner } from '#core/types/generated/CompareOwner.ts';

export const autoScanTicketLedgerCapacity = 64;

export interface AutoScanTicket {
  generation: number;
  ticketId: number;
  jobId: string;
  jobName: string;
  configRevision: string;
  targetIndex: number;
  autoApply: boolean;
}

export type AutoScanStatusSource = 'event' | 'snapshot' | 'start' | 'decline' | 'stop';

function sameBinding(left: AutoScanStatusDto, right: AutoScanStatusDto): boolean {
  if (left.job_id === null || right.job_id === null) return !left.active && !right.active;
  return left.job_id === right.job_id
    && left.config_revision === right.config_revision
    && left.target_index === right.target_index;
}

function statusRank(status: AutoScanStatusDto): number {
  if (!status.active) return 4;
  if (status.pending_trigger !== null || status.active_ticket !== null) return 2;
  if (status.latest_ticket_id > 0) return 3;
  return status.mode === 'starting' ? 0 : 1;
}

function statusMatchesTicket(status: AutoScanStatusDto, ticket: AutoScanTicket): boolean {
  return status.active
    && status.generation === ticket.generation
    && status.job_id === ticket.jobId
    && status.config_revision === ticket.configRevision
    && status.target_index === ticket.targetIndex
    && status.latest_ticket_id === ticket.ticketId;
}

function localTicketMatches(active: AutoScanTicket | null, ticket: AutoScanTicket): boolean {
  return active !== null
    && active.generation === ticket.generation
    && active.ticketId === ticket.ticketId
    && active.jobId === ticket.jobId
    && active.configRevision === ticket.configRevision
    && active.targetIndex === ticket.targetIndex;
}

/**
 * Reconcile snapshots, events, and command responses without allowing an older worker generation
 * or ticket cursor to resurrect or regress the monitor shown in the toolbar.
 */
export function reconcileAutoScanStatus(
  current: AutoScanStatusDto | null,
  incoming: AutoScanStatusDto,
  source: AutoScanStatusSource,
  declinedTicket?: AutoScanTicket,
): AutoScanStatusDto | null {
  if (source === 'decline') {
    const exactDecline = declinedTicket !== undefined
      && incoming.generation === declinedTicket.generation
      && incoming.job_id === declinedTicket.jobId
      && incoming.config_revision === declinedTicket.configRevision
      && incoming.target_index === declinedTicket.targetIndex
      && incoming.latest_ticket_id === declinedTicket.ticketId
      && incoming.active_ticket === null
      && incoming.pending_trigger === null;
    if (!exactDecline) return current;
  }
  if (!current) return incoming;

  if (incoming.generation < current.generation) return current;
  if (incoming.generation > current.generation) return incoming;
  if (!sameBinding(current, incoming)) return current;
  if (!current.active && incoming.active) return current;
  if (incoming.latest_ticket_id < current.latest_ticket_id) return current;
  if (source === 'decline' && current.latest_ticket_id > (declinedTicket?.ticketId ?? -1)) return current;
  if (incoming.latest_ticket_id > current.latest_ticket_id) return incoming;
  if (source === 'decline') {
    const currentTicketId = current.pending_trigger?.ticket_id ?? current.active_ticket;
    if (currentTicketId !== null && currentTicketId !== declinedTicket?.ticketId) return current;
  }
  if (statusRank(incoming) < statusRank(current)) return current;
  return incoming;
}

export function monitorOwnsAutoScanResult(
  status: AutoScanStatusDto | null,
  active: AutoScanTicket | null,
  ticket: AutoScanTicket,
  owner: CompareOwner,
): boolean {
  return statusCompletesAutoScanTicket(status, ticket)
    && localTicketMatches(active, ticket)
    && owner.identity.job_id === ticket.jobId
    && owner.identity.config_revision === ticket.configRevision
    && owner.identity.target_index === ticket.targetIndex;
}

export function statusCompletesAutoScanTicket(
  status: AutoScanStatusDto | null,
  ticket: AutoScanTicket,
): boolean {
  return status !== null
    && statusMatchesTicket(status, ticket)
    && status.active_ticket === null
    && status.pending_trigger === null;
}

export function monitorOwnsAutoScanTicket(
  status: AutoScanStatusDto | null,
  active: AutoScanTicket | null,
  ticket: AutoScanTicket,
): boolean {
  return statusCanOwnAutoScanTrigger(status, ticket) && localTicketMatches(active, ticket);
}

export function statusCanOwnAutoScanTrigger(
  status: AutoScanStatusDto | null,
  ticket: AutoScanTicket,
): boolean {
  if (status === null || !statusMatchesTicket(status, ticket)) return false;
  const pending = status.pending_trigger;
  return status.active_ticket === ticket.ticketId
    && pending !== null
    && pending.generation === ticket.generation
    && pending.ticket_id === ticket.ticketId
    && pending.job_id === ticket.jobId
    && pending.config_revision === ticket.configRevision
    && pending.target_index === ticket.targetIndex;
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

type LedgerRecord =
  | { stage: 'processing' }
  | { stage: 'decline_recovery' }
  | { stage: 'completed' };

export type AutoScanTicketClaim =
  | { kind: 'process' }
  | { kind: 'decline_recovery' }
  | { kind: 'duplicate' }
  | { kind: 'capacity' };

/**
 * Bounded, generation/ticket keyed at-most-once processing. Only an authoritative recovered
 * pending trigger can re-enter a claimed ticket, and it can re-enter solely to decline it.
 */
export class AutoScanTicketLedger {
  readonly #capacity: number;
  readonly #records = new Map<number, LedgerRecord>();
  #generation: number | null = null;
  #completedThroughTicketId = 0;

  constructor(capacity = autoScanTicketLedgerCapacity) {
    if (!Number.isInteger(capacity) || capacity < 1) throw new Error('AutoScan ledger capacity must be positive');
    this.#capacity = capacity;
  }

  get size(): number { return this.#records.size; }

  claim(ticket: Pick<AutoScanTicket, 'generation' | 'ticketId'>): AutoScanTicketClaim {
    if (this.#generation !== null && ticket.generation < this.#generation) return { kind: 'duplicate' };
    if (this.#generation === null || ticket.generation > this.#generation) {
      this.#generation = ticket.generation;
      this.#completedThroughTicketId = 0;
      this.#records.clear();
    }

    const existing = this.#records.get(ticket.ticketId);
    if (existing?.stage === 'decline_recovery') {
      this.#records.set(ticket.ticketId, { stage: 'processing' });
      return { kind: 'decline_recovery' };
    }
    if (existing || ticket.ticketId <= this.#completedThroughTicketId) return { kind: 'duplicate' };
    this.#trimCompleted();
    if (this.#records.size >= this.#capacity) return { kind: 'capacity' };
    this.#records.set(ticket.ticketId, { stage: 'processing' });
    return { kind: 'process' };
  }

  markDeclineRecovery(ticket: Pick<AutoScanTicket, 'generation' | 'ticketId'>): boolean {
    if (ticket.generation !== this.#generation) return false;
    if (this.#records.get(ticket.ticketId)?.stage !== 'processing') return false;
    this.#records.set(ticket.ticketId, { stage: 'decline_recovery' });
    return true;
  }

  markCompleted(ticket: Pick<AutoScanTicket, 'generation' | 'ticketId'>): boolean {
    if (ticket.generation !== this.#generation || !this.#records.has(ticket.ticketId)) return false;
    this.#records.set(ticket.ticketId, { stage: 'completed' });
    this.#completedThroughTicketId = Math.max(this.#completedThroughTicketId, ticket.ticketId);
    return true;
  }

  #trimCompleted(): void {
    while (this.#records.size >= this.#capacity) {
      const completed = [...this.#records].find(([, record]) => record.stage === 'completed');
      if (!completed) return;
      this.#records.delete(completed[0]);
    }
  }
}
