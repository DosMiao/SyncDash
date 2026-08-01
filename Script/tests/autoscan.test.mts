import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';

import {
  AutoScanTicketLedger,
  autoScanButtonLabel,
  autoScanToggleAction,
  monitorOwnsAutoScanResult,
  reconcileAutoScanStatus,
  statusCanOwnAutoScanTrigger,
} from '../../typescript/ui/state/autoscan.ts';
import type { AutoScanStatusDto } from '../../typescript/core/ipc.ts';
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

function status(overrides: Partial<AutoScanStatusDto> = {}): AutoScanStatusDto {
  return {
    active: true,
    generation: 9,
    job_id: 'job-id-photos',
    job_name: 'photos',
    config_revision: 'rev-2',
    target_index: 1,
    interval_secs: 30,
    auto_apply: true,
    mode: 'polling',
    detail: 'Watching photos',
    active_ticket: null,
    latest_ticket_id: 3,
    pending_trigger: null,
    ...overrides,
  };
}

const owner = {
  identity: {
    compare_run_id: 17,
    job_id: 'job-id-photos',
    config_revision: 'rev-2',
    target_index: 1,
  },
  job_name: 'photos',
};

test('monitor ownership is independent of whichever job or target the user is viewing', () => {
  assert.equal(monitorOwnsAutoScanResult(status(), ticket, ticket, owner), true);
  assert.equal(monitorOwnsAutoScanResult(status(), null, ticket, owner), false);
  assert.equal(monitorOwnsAutoScanResult(status({ active: false }), ticket, ticket, owner), false);
  assert.equal(monitorOwnsAutoScanResult(status({ generation: 10 }), ticket, ticket, owner), false);
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
  assert.equal(statusCanOwnAutoScanTrigger(status({ latest_ticket_id: 4 }), ticket), false);
});

test('status reconciliation rejects stale generations and regressions but accepts terminal events', () => {
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

  const staleInactive = status({
    active: false, generation: 0, job_id: null, job_name: null, config_revision: null,
    target_index: null, interval_secs: null, auto_apply: false, mode: null,
    latest_ticket_id: 0,
  });
  assert.equal(reconcileAutoScanStatus(current, staleInactive, 'snapshot'), current);
  assert.equal(reconcileAutoScanStatus(current, status({ generation: 11 }), 'completion', 3), current);
  const orderedInactive = status({ active: false, generation: 12, mode: null, detail: 'Stopped', latest_ticket_id: 0 });
  assert.equal(reconcileAutoScanStatus(current, orderedInactive, 'snapshot'), orderedInactive);
  const stopped = reconcileAutoScanStatus(current, staleInactive, 'event');
  assert.equal(stopped?.active, false);
  assert.equal(stopped?.generation, 12);
  assert.equal(reconcileAutoScanStatus(stopped, current, 'event'), stopped);
});

test('ticket cursor prevents delayed completion and trigger reordering within one generation', () => {
  const pendingFour = status({
    latest_ticket_id: 4,
    active_ticket: 4,
    pending_trigger: { generation: 9, ticket_id: 4, job_id: ticket.jobId, job_name: ticket.jobName,
      config_revision: ticket.configRevision, target_index: ticket.targetIndex, auto_apply: true,
      mode: 'polling', reason: 'periodic_verification' },
  });
  const completedThree = status({ latest_ticket_id: 3, active_ticket: null, pending_trigger: null });
  assert.equal(reconcileAutoScanStatus(pendingFour, completedThree, 'completion', 3), pendingFour);

  const completedFour = status({ latest_ticket_id: 4, active_ticket: null, pending_trigger: null });
  assert.equal(reconcileAutoScanStatus(pendingFour, completedFour, 'completion', 4), completedFour);
  assert.equal(reconcileAutoScanStatus(completedFour, pendingFour, 'event'), completedFour);
});

test('event plus recovered pending trigger never processes a ticket twice', () => {
  const ledger = new AutoScanTicketLedger<{ succeeded: boolean }>(4);
  assert.deepEqual(ledger.claim(ticket), { kind: 'process' });
  assert.deepEqual(ledger.claim(ticket), { kind: 'duplicate' });
  assert.equal(ledger.prepareCompletion(ticket, { succeeded: true }), true);
  ledger.completionFailed(ticket);
  assert.deepEqual(ledger.claim(ticket), { kind: 'retry_completion', outcome: { succeeded: true } });
  assert.deepEqual(ledger.claim(ticket), { kind: 'duplicate' });
  ledger.completed(ticket);
  assert.deepEqual(ledger.claim(ticket), { kind: 'duplicate' });
});

test('ticket ledger stays bounded without evicting in-flight work', () => {
  const ledger = new AutoScanTicketLedger<boolean>(2);
  const first = { ...ticket, ticketId: 1 };
  const second = { ...ticket, ticketId: 2 };
  const third = { ...ticket, ticketId: 3 };
  assert.equal(ledger.claim(first).kind, 'process');
  assert.equal(ledger.claim(second).kind, 'process');
  assert.equal(ledger.claim(third).kind, 'capacity');
  assert.equal(ledger.size, 2);
  ledger.prepareCompletion(first, true);
  ledger.completed(first);
  assert.equal(ledger.claim(first).kind, 'duplicate');
  assert.equal(ledger.claim(third).kind, 'process');
  assert.equal(ledger.size, 2);
});

test('toolbar labels the monitored job and remains stoppable without a selected job', () => {
  assert.equal(autoScanButtonLabel(status(), null), 'AutoScan · photos · T2');
  assert.equal(autoScanToggleAction(status(), false), 'stop');
  assert.equal(autoScanToggleAction(null, true), 'start');
  assert.equal(autoScanToggleAction(null, false), 'unavailable');
});

test('App has no navigation/mutation stop or UI-derived unattended payload', async () => {
  const app = await readFile(new URL('../../typescript/ui/App.tsx', import.meta.url), 'utf8');
  assert.doesNotMatch(app, /authorizeUnattendedApply|visibleForCycle/);
  assert.doesNotMatch(app, /selectedRows\([\s\S]{0,300}authorizeAutoScanApply/);
  assert.equal(app.match(/\bstopAutoScan\(\);/g)?.length, 1, 'only the explicit toolbar toggle may call the UI stop helper');
  assert.equal(app.match(/ipc\.stopAutoScan\(\)/g)?.length, 1, 'only an explicit stop crosses IPC');
  assert.match(app, /autoScanControlPendingRef\.current !== null/, 'control clicks are fenced synchronously');
});
