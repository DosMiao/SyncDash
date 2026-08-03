import type { AuthorizationDto } from '#core/types/generated/AuthorizationDto.ts';
import type { Dispatch, MutableRefObject, SetStateAction } from 'react';
import * as applyIpc from '#core/infrastructure/tauri/commands/apply.ts';
import * as autoscanIpc from '#core/infrastructure/tauri/commands/autoscan.ts';
import {
  monitorOwnsAutoScanResult,
  statusCanOwnAutoScanTrigger,
  statusCompletesAutoScanTicket,
} from '#core/application/autoscan/autoscan.ts';
import type { AutoScanTicket } from '#core/application/autoscan/autoscan.ts';
import type { AutoScanStatusSource } from '#core/application/autoscan/autoscan.ts';
import type { ReviewRequestFence } from '#core/application/operations/operationReview.ts';
import {
  interactionBlocksUnattendedWrite,
  interactionConflictsWithReservedWrite,
} from '#core/application/safety/executionSafety.ts';
import type { ExecutionInteractionState } from '#core/application/safety/executionSafety.ts';
import type { AutoScanStatusDto } from '#core/types/generated/AutoScanStatusDto.ts';
import type { CompareCompletion } from '../../model/workspacePageModel.ts';
import type { AutoScanRuntimeState } from './useAutoScanState.ts';
import { useAutoScanSubscriptions } from './useAutoScanSubscriptions.ts';

type StatusClass = '' | 'ok' | 'err';
type SetStatus = (message: string, statusClass?: StatusClass) => void;

interface AutoScanTicketLifecycleOptions {
  runtime: AutoScanRuntimeState;
  acceptStatus: (
    status: AutoScanStatusDto,
    source: AutoScanStatusSource,
    declinedTicket?: AutoScanTicket,
  ) => boolean;
  doCompare: (ticket: AutoScanTicket) => Promise<CompareCompletion | null>;
  compareInFlightRef: MutableRefObject<boolean>;
  applyExecutionRequestRef: MutableRefObject<ReviewRequestFence | null>;
  applyReviewRequestRef: MutableRefObject<ReviewRequestFence | null>;
  compareReviewRequestRef: MutableRefObject<ReviewRequestFence | null>;
  autoApplyInFlightRef: MutableRefObject<boolean>;
  liveInteractionStateRef: MutableRefObject<ExecutionInteractionState>;
  setBusy: Dispatch<SetStateAction<boolean>>;
  setLogReload: Dispatch<SetStateAction<number>>;
  refreshLatestRunSummaries: () => void;
  setWorkspaceStatus: SetStatus;
}

export function useAutoScanTicketLifecycle({
  runtime,
  acceptStatus,
  doCompare,
  compareInFlightRef,
  applyExecutionRequestRef,
  applyReviewRequestRef,
  compareReviewRequestRef,
  autoApplyInFlightRef,
  liveInteractionStateRef,
  setBusy,
  setLogReload,
  refreshLatestRunSummaries,
  setWorkspaceStatus,
}: AutoScanTicketLifecycleOptions) {
  const { statusRef, ticketRef, ledgerRef, triggerRef } = runtime;

  triggerRef.current = async (trigger) => {
    const ticket: AutoScanTicket = {
      generation: trigger.generation,
      ticketId: trigger.ticket_id,
      jobId: trigger.job_id,
      jobName: trigger.job_name,
      configRevision: trigger.config_revision,
      targetIndex: trigger.target_index,
      autoApply: trigger.auto_apply,
    };
    const clearLocalTicket = () => {
      if (ticketRef.current?.generation === ticket.generation
        && ticketRef.current.ticketId === ticket.ticketId) {
        ticketRef.current = null;
      }
    };
    const markCompleted = () => {
      ledgerRef.current.markCompleted(ticket);
      clearLocalTicket();
    };
    const recoverOrDecline = async (reason: string) => {
      let observed;
      try {
        observed = await autoscanIpc.autoScanStatus();
        acceptStatus(observed, 'snapshot');
        observed = statusRef.current ?? observed;
      } catch (error) {
        ledgerRef.current.markDeclineRecovery(ticket);
        clearLocalTicket();
        setWorkspaceStatus(`${reason}; AutoScan could not verify whether the pending ticket needs release: ${error}`, 'err');
        return;
      }
      if (statusCompletesAutoScanTicket(observed, ticket)) {
        markCompleted();
        return;
      }
      if (!statusCanOwnAutoScanTrigger(observed, ticket)) {
        markCompleted();
        return;
      }
      try {
        const declined = await autoscanIpc.declineAutoScanTrigger(ticket.generation, ticket.ticketId);
        acceptStatus(declined, 'decline', ticket);
        const authoritative = statusRef.current ?? declined;
        if (statusCompletesAutoScanTicket(authoritative, ticket)) {
          markCompleted();
          setWorkspaceStatus(`${reason}; this AutoScan cycle was released without launching another Compare`, 'err');
          return;
        }
        if (!statusCanOwnAutoScanTrigger(authoritative, ticket)) {
          markCompleted();
          return;
        }
      } catch (declineError) {
        try {
          const recovered = await autoscanIpc.autoScanStatus();
          acceptStatus(recovered, 'snapshot');
          const authoritative = statusRef.current ?? recovered;
          if (statusCompletesAutoScanTicket(authoritative, ticket)) {
            markCompleted();
            return;
          }
          if (!statusCanOwnAutoScanTrigger(authoritative, ticket)) {
            markCompleted();
            return;
          }
        } catch (statusError) {
          ledgerRef.current.markDeclineRecovery(ticket);
          clearLocalTicket();
          setWorkspaceStatus(`${reason}; decline failed (${declineError}) and recovery status failed (${statusError})`, 'err');
          return;
        }
        ledgerRef.current.markDeclineRecovery(ticket);
        clearLocalTicket();
        setWorkspaceStatus(`${reason}; the ticket is still backend-owned and will be declined from a recovered trigger: ${declineError}`, 'err');
        return;
      }
      ledgerRef.current.markDeclineRecovery(ticket);
      clearLocalTicket();
      setWorkspaceStatus(`${reason}; the decline response did not prove exact terminal ownership`, 'err');
    };

    const claim = ledgerRef.current.claim(ticket);
    if (claim.kind === 'duplicate') return;
    if (claim.kind === 'capacity') {
      await recoverOrDecline('AutoScan rejected a trigger because its bounded recovery ledger is full');
      return;
    }

    const observed = statusRef.current;
    if (observed?.active
      && observed.generation === ticket.generation
      && observed.job_id === ticket.jobId
      && observed.config_revision === ticket.configRevision
      && observed.target_index === ticket.targetIndex) {
      // The trigger event is itself the newest backend cursor. Materialize it into status so a
      // delayed completion for N cannot race an event for N+1 merely because no status event exists.
      acceptStatus({
        ...observed,
        latest_ticket_id: ticket.ticketId,
        active_ticket: ticket.ticketId,
        pending_trigger: trigger,
        mode: trigger.mode,
      }, 'event');
    }
    if (claim.kind === 'decline_recovery') {
      await recoverOrDecline('AutoScan recovered a ticket whose earlier decline was not confirmed');
      return;
    }

    let monitor = statusRef.current;
    if (!statusCanOwnAutoScanTrigger(monitor, ticket)) {
      try {
        const snapshot = await autoscanIpc.autoScanStatus();
        acceptStatus(snapshot, 'snapshot');
        monitor = statusRef.current;
      } catch (error) {
        setWorkspaceStatus(`AutoScan could not verify trigger ownership: ${error}`, 'err');
      }
    }
    if (!statusCanOwnAutoScanTrigger(monitor, ticket)) {
      await recoverOrDecline('AutoScan refused a trigger that no longer had exact backend ownership');
      return;
    }

    ticketRef.current = ticket;
    const completion = await doCompare(ticket);
    if (!completion) {
      await recoverOrDecline('AutoScan Compare did not publish a result');
      return;
    }

    let publishedStatus;
    try {
      publishedStatus = await autoscanIpc.autoScanStatus();
      acceptStatus(publishedStatus, 'snapshot');
      publishedStatus = statusRef.current ?? publishedStatus;
    } catch (error) {
      markCompleted();
      setWorkspaceStatus(`AutoScan retained the Compare result but could not verify its terminal status, so AutoApply was not attempted: ${error}`, 'err');
      return;
    }
    const ownsPublishedResult = monitorOwnsAutoScanResult(
      publishedStatus,
      ticketRef.current,
      ticket,
      completion.plan.owner,
    );
    const terminalStatus = statusCompletesAutoScanTicket(publishedStatus, ticket);
    markCompleted();
    if (!terminalStatus) {
      setWorkspaceStatus('AutoScan published a result, but its status cursor moved before AutoApply ownership could be proven; review the retained result', 'err');
      return;
    }
    if (!ownsPublishedResult) return;

    const freshPlan = completion.plan;
    if (freshPlan.ops.length === 0) return;
    if (!ticket.autoApply) {
      setWorkspaceStatus(`AutoScan found ${freshPlan.ops.length} differences — review required`, 'err');
      return;
    }

    const interaction = liveInteractionStateRef.current;
    if (compareInFlightRef.current
      || applyExecutionRequestRef.current
      || applyReviewRequestRef.current
      || compareReviewRequestRef.current
      || interactionBlocksUnattendedWrite(interaction)) {
      setWorkspaceStatus(`AutoScan found ${freshPlan.ops.length} differences — another interaction owns execution; review required`, 'err');
      return;
    }
    const applyStatus = statusRef.current;
    if (applyStatus?.active !== true
      || applyStatus.generation !== ticket.generation
      || applyStatus.latest_ticket_id !== ticket.ticketId
      || applyStatus.job_id !== ticket.jobId
      || applyStatus.config_revision !== ticket.configRevision
      || applyStatus.target_index !== ticket.targetIndex
      || applyStatus.active_ticket !== null
      || applyStatus.pending_trigger !== null
      || ticketRef.current !== null) return;
    setWorkspaceStatus(`AutoScan found ${freshPlan.ops.length} differences — checking the backend-owned AutoApply ticket…`);
    autoApplyInFlightRef.current = true;
    setBusy(true);
    try {
      let authorization: AuthorizationDto;
      try {
        authorization = await autoscanIpc.authorizeAutoScanApply(ticket.generation, ticket.ticketId);
      } catch (error) {
        setWorkspaceStatus(
          `AutoApply did not run: interactive review is required for this exact job revision, target, and capability set: ${error}`,
          'err',
        );
        return;
      }
      if (interactionConflictsWithReservedWrite(liveInteractionStateRef.current)
        || compareInFlightRef.current
        || applyExecutionRequestRef.current
        || applyReviewRequestRef.current
        || compareReviewRequestRef.current) {
        setWorkspaceStatus('AutoApply did not run because another interaction opened during authorization; review the retained result', 'err');
        return;
      }
      try {
        const result = await applyIpc.applyJob(authorization.authorization_token);
        refreshLatestRunSummaries();
        setLogReload((value) => value + 1);
        setWorkspaceStatus(
          result.cancelled
            ? `Auto-sync stopped after ${result.done} actions`
            : `Auto-sync finished: ${result.done} run, ${result.skipped} skipped, ${result.errors} errors`,
          result.errors ? 'err' : 'ok',
        );
      } catch (error) {
        setWorkspaceStatus(
          `The authorized auto-sync failed and may have made partial changes: ${error} — Compare again before continuing`,
          'err',
        );
      }
    } finally {
      autoApplyInFlightRef.current = false;
      setBusy(false);
    }
  };

  useAutoScanSubscriptions({
    acceptStatus,
    trigger: triggerRef,
    setStatus: setWorkspaceStatus,
  });
}
