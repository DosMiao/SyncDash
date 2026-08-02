import assert from 'node:assert/strict';
import test from 'node:test';

import {
  AutoScanTicketLedger,
  autoScanButtonLabel,
  autoScanToggleAction,
  monitorOwnsAutoScanResult,
  monitorOwnsAutoScanTicket,
  reconcileAutoScanStatus,
  statusCanOwnAutoScanTrigger,
  statusCompletesAutoScanTicket,
} from '../../typescript/ui/state/autoscan.ts';
import type { AutoScanStatusDto } from '../../typescript/core/types/generated/AutoScanStatusDto.ts';
import type { AutoScanTriggerDto } from '../../typescript/core/types/generated/AutoScanTriggerDto.ts';
import type { AutoScanTicket } from '../../typescript/ui/state/autoscan.ts';

const ticket: AutoScanTicket = {
  generation: 9,
  ticketId: 3,
  jobId: 'job-id-photos',
  jobName: 'photos',
  configRevision: 'rev-2',
  targetIndex: 1,
  autoApply: true,
};

function trigger(overrides: Partial<AutoScanTriggerDto> = {}): AutoScanTriggerDto {
  return {
    generation: ticket.generation,
    ticket_id: ticket.ticketId,
    job_id: ticket.jobId,
    job_name: ticket.jobName,
    config_revision: ticket.configRevision,
    target_index: ticket.targetIndex,
    auto_apply: ticket.autoApply,
    mode: 'polling',
    reason: 'periodic_verification',
    ...overrides,
  };
}

function status(overrides: Partial<AutoScanStatusDto> = {}): AutoScanStatusDto {
  return {
    active: true,
    generation: ticket.generation,
    job_id: ticket.jobId,
    job_name: ticket.jobName,
    config_revision: ticket.configRevision,
    target_index: ticket.targetIndex,
    interval_secs: 30,
    auto_apply: true,
    mode: 'polling',
    detail: 'Watching photos',
    active_ticket: null,
    latest_ticket_id: ticket.ticketId,
    pending_trigger: null,
    ...overrides,
  };
}

function pendingStatus(ticketId = ticket.ticketId, overrides: Partial<AutoScanStatusDto> = {}): AutoScanStatusDto {
  return status({
    latest_ticket_id: ticketId,
    active_ticket: ticketId,
    pending_trigger: trigger({ ticket_id: ticketId }),
    ...overrides,
  });
}

const owner = {
  identity: {
    result_id: '11111111111111111111111111111111',
    compare_run_id: 17,
    job_id: ticket.jobId,
    config_revision: ticket.configRevision,
    target_index: ticket.targetIndex,
  },
  job_name: ticket.jobName,
};

test('published result ownership requires one exact completed ticket and result identity', () => {
  const published = status();
  assert.equal(statusCompletesAutoScanTicket(published, ticket), true);
  assert.equal(statusCompletesAutoScanTicket(pendingStatus(), ticket), false);
  assert.equal(statusCompletesAutoScanTicket(status({ latest_ticket_id: 4 }), ticket), false);
  assert.equal(monitorOwnsAutoScanResult(published, ticket, ticket, owner), true);
  assert.equal(monitorOwnsAutoScanResult(pendingStatus(), ticket, ticket, owner), false);
  assert.equal(monitorOwnsAutoScanResult(status({ active: false }), ticket, ticket, owner), false);
  assert.equal(monitorOwnsAutoScanResult(status({ generation: 10 }), ticket, ticket, owner), false);
  assert.equal(monitorOwnsAutoScanResult(status({ latest_ticket_id: 4 }), ticket, ticket, owner), false);
  assert.equal(monitorOwnsAutoScanResult(status(), { ...ticket, ticketId: 4 }, ticket, owner), false);
  assert.equal(monitorOwnsAutoScanResult(status(), ticket, ticket, {
    ...owner,
    identity: { ...owner.identity, job_id: 'replacement' },
  }), false);
  assert.equal(monitorOwnsAutoScanResult(status(), ticket, ticket, {
    ...owner,
    identity: { ...owner.identity, config_revision: 'old' },
  }), false);
  assert.equal(monitorOwnsAutoScanResult(status(), ticket, ticket, {
    ...owner,
    identity: { ...owner.identity, target_index: 0 },
  }), false);
});

test('pending trigger ownership requires exact status, trigger, and local ticket cursors', () => {
  const pending = pendingStatus();
  assert.equal(statusCanOwnAutoScanTrigger(pending, ticket), true);
  assert.equal(monitorOwnsAutoScanTicket(pending, ticket, ticket), true);
  assert.equal(statusCanOwnAutoScanTrigger(status(), ticket), false);
  assert.equal(statusCanOwnAutoScanTrigger(pendingStatus(4), ticket), false);
  assert.equal(statusCanOwnAutoScanTrigger(pendingStatus(3, {
    pending_trigger: trigger({ config_revision: 'stale-revision' }),
  }), ticket), false);
  assert.equal(monitorOwnsAutoScanTicket(pending, null, ticket), false);
  assert.equal(monitorOwnsAutoScanTicket(pending, { ...ticket, targetIndex: 0 }, ticket), false);
});

test('status reconciliation rejects stale generations and monitor regressions', () => {
  const current = status({ generation: 12, mode: 'polling', detail: 'ready', latest_ticket_id: 0 });
  assert.equal(reconcileAutoScanStatus(current, status({ generation: 11 }), 'event'), current);
  assert.equal(
    reconcileAutoScanStatus(current, status({ generation: 12, mode: 'starting', latest_ticket_id: 0 }), 'start'),
    current,
  );
  assert.equal(
    reconcileAutoScanStatus(current, status({ generation: 12, job_id: 'replacement' }), 'event'),
    current,
  );

  const neverArmed = status({
    active: false,
    generation: 0,
    job_id: null,
    job_name: null,
    config_revision: null,
    target_index: null,
    interval_secs: null,
    auto_apply: false,
    mode: null,
    latest_ticket_id: 0,
  });
  assert.equal(reconcileAutoScanStatus(current, neverArmed, 'snapshot'), current);

  const stopped = status({
    active: false,
    generation: 12,
    mode: null,
    detail: 'Stopped',
    latest_ticket_id: 0,
  });
  assert.equal(reconcileAutoScanStatus(current, stopped, 'stop'), stopped);
  assert.equal(reconcileAutoScanStatus(stopped, current, 'event'), stopped);
  assert.equal(reconcileAutoScanStatus(current, status({ generation: 13, latest_ticket_id: 0 }), 'start')?.generation, 13);
});

test('published snapshots and trigger events remain monotonic under every delivery order', () => {
  const pendingThree = pendingStatus(3);
  const publishedThree = status({ latest_ticket_id: 3 });
  assert.equal(reconcileAutoScanStatus(pendingThree, publishedThree, 'snapshot'), publishedThree);
  assert.equal(reconcileAutoScanStatus(publishedThree, pendingThree, 'event'), publishedThree);

  const pendingFour = pendingStatus(4);
  assert.equal(reconcileAutoScanStatus(publishedThree, pendingFour, 'event'), pendingFour);
  assert.equal(reconcileAutoScanStatus(pendingFour, publishedThree, 'snapshot'), pendingFour);

  const publishedFour = status({ latest_ticket_id: 4 });
  assert.equal(reconcileAutoScanStatus(pendingFour, publishedFour, 'snapshot'), publishedFour);
  assert.equal(reconcileAutoScanStatus(publishedFour, pendingFour, 'event'), publishedFour);
});

test('decline responses are accepted only for the exact pending ticket', () => {
  const pendingFour = pendingStatus(4);
  const declinedFour = status({ latest_ticket_id: 4, detail: 'Trigger declined' });
  const ticketFour = { ...ticket, ticketId: 4 };
  assert.equal(reconcileAutoScanStatus(pendingFour, declinedFour, 'decline', { ...ticketFour, ticketId: 3 }), pendingFour);
  assert.equal(reconcileAutoScanStatus(pendingFour, declinedFour, 'decline'), pendingFour);
  assert.equal(reconcileAutoScanStatus(pendingFour, status({ latest_ticket_id: 5 }), 'decline', ticketFour), pendingFour);
  assert.equal(reconcileAutoScanStatus(pendingFour, declinedFour, 'decline', ticketFour), declinedFour);

  const pendingFive = pendingStatus(5);
  assert.equal(reconcileAutoScanStatus(pendingFive, declinedFour, 'decline', ticketFour), pendingFive);
  assert.equal(reconcileAutoScanStatus(null, declinedFour, 'decline', ticketFour), declinedFour);
  assert.equal(reconcileAutoScanStatus(null, pendingFour, 'decline', ticketFour), null);
  assert.equal(reconcileAutoScanStatus(null, declinedFour, 'decline', { ...ticketFour, generation: 8 }), null);
});

test('a recovered prelaunch decline can retry without rerunning Compare', () => {
  const ledger = new AutoScanTicketLedger(4);
  let processCount = 0;
  const first = ledger.claim(ticket);
  if (first.kind === 'process') processCount += 1;
  assert.deepEqual(first, { kind: 'process' });

  assert.equal(ledger.markDeclineRecovery(ticket), true);
  assert.deepEqual(ledger.claim(ticket), { kind: 'decline_recovery' });
  assert.deepEqual(ledger.claim(ticket), { kind: 'duplicate' });

  assert.equal(ledger.markDeclineRecovery(ticket), true);
  assert.deepEqual(ledger.claim(ticket), { kind: 'decline_recovery' });
  assert.equal(ledger.markCompleted(ticket), true);
  assert.deepEqual(ledger.claim(ticket), { kind: 'duplicate' });
  assert.equal(processCount, 1);
});

test('successful publication tombstones a ticket so reordered notifications never rerun it', () => {
  const ledger = new AutoScanTicketLedger(2);
  assert.deepEqual(ledger.claim(ticket), { kind: 'process' });
  assert.equal(ledger.markCompleted(ticket), true);
  assert.deepEqual(ledger.claim(ticket), { kind: 'duplicate' });
  assert.deepEqual(ledger.claim(ticket), { kind: 'duplicate' });
  assert.equal(ledger.markDeclineRecovery(ticket), false);
});

test('ticket ledger is bounded, preserves live recovery, and rejects retired cursors', () => {
  const ledger = new AutoScanTicketLedger(2);
  const first = { ...ticket, ticketId: 1 };
  const second = { ...ticket, ticketId: 2 };
  const third = { ...ticket, ticketId: 3 };
  assert.equal(ledger.claim(first).kind, 'process');
  assert.equal(ledger.claim(second).kind, 'process');
  assert.equal(ledger.markDeclineRecovery(second), true);
  assert.equal(ledger.claim(third).kind, 'capacity');
  assert.equal(ledger.size, 2);

  assert.equal(ledger.markCompleted(first), true);
  assert.equal(ledger.claim(third).kind, 'process');
  assert.equal(ledger.size, 2);
  assert.equal(ledger.claim(first).kind, 'duplicate');

  const nextGeneration = { ...ticket, generation: 10, ticketId: 1 };
  assert.equal(ledger.claim(nextGeneration).kind, 'process');
  assert.equal(ledger.size, 1);
  assert.equal(ledger.claim(third).kind, 'duplicate');
});

test('toolbar labels the monitored job and remains stoppable without a selected job', () => {
  assert.equal(autoScanButtonLabel(status(), null), 'AutoScan · photos · T2');
  assert.equal(autoScanToggleAction(status(), false), 'stop');
  assert.equal(autoScanToggleAction(null, true), 'start');
  assert.equal(autoScanToggleAction(null, false), 'unavailable');
});
