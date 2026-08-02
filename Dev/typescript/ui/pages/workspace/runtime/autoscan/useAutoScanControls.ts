import { useCallback } from 'react';
import * as ipc from '#core/infrastructure/tauri/commands/main.ts';
import {
  autoScanToggleAction,
  reconcileAutoScanStatus,
} from '#core/application/autoscan/autoscan.ts';
import type {
  AutoScanStatusSource,
  AutoScanTicket,
} from '#core/application/autoscan/autoscan.ts';
import type { AutoScanStatusDto } from '#core/types/generated/AutoScanStatusDto.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { AutoScanRuntimeState } from './useAutoScanState.ts';

type StatusClass = '' | 'ok' | 'err';
type SetStatus = (message: string, statusClass?: StatusClass) => void;

interface AutoScanControlsOptions {
  runtime: AutoScanRuntimeState;
  selectedJob: JobDto | null;
  selectedTargetIndex: number;
  setWorkspaceStatus: SetStatus;
}

export function useAutoScanControls({
  runtime,
  selectedJob,
  selectedTargetIndex,
  setWorkspaceStatus,
}: AutoScanControlsOptions) {
  const {
    statusRef,
    setStatus,
    triggerRef,
    controlPendingRef,
    controlRequestRef,
    setControlPending,
    ticketRef,
  } = runtime;
  const acceptStatus = useCallback((
    incoming: AutoScanStatusDto,
    source: AutoScanStatusSource,
    declinedTicket?: AutoScanTicket,
  ) => {
    const current = statusRef.current;
    const next = reconcileAutoScanStatus(current, incoming, source, declinedTicket);
    if (!next || next === current) return false;
    statusRef.current = next;
    setStatus(next);
    if (next.pending_trigger) void triggerRef.current(next.pending_trigger);
    return true;
  }, [setStatus, statusRef, triggerRef]);

  const stop = useCallback(() => {
    if (controlPendingRef.current !== null) return;
    const request = controlRequestRef.current + 1;
    controlRequestRef.current = request;
    controlPendingRef.current = 'stop';
    setControlPending('stop');
    setWorkspaceStatus('Stopping AutoScan…');
    void ipc.stopAutoScan().then((next) => {
      if (controlRequestRef.current !== request) return;
      acceptStatus(next, 'stop');
      ticketRef.current = null;
      setWorkspaceStatus('AutoScan stopped');
    }).catch((error) => {
      if (controlRequestRef.current === request) {
        setWorkspaceStatus(`AutoScan could not be stopped cleanly: ${error}`, 'err');
      }
    }).finally(() => {
      if (controlRequestRef.current === request) {
        controlPendingRef.current = null;
        setControlPending(null);
      }
    });
  }, [
    acceptStatus,
    controlPendingRef,
    controlRequestRef,
    setControlPending,
    setWorkspaceStatus,
    ticketRef,
  ]);

  const toggle = () => {
    const action = autoScanToggleAction(statusRef.current, selectedJob !== null);
    if (action === 'stop') {
      stop();
      return;
    }
    if (action !== 'start' || !selectedJob || controlPendingRef.current !== null) return;
    const monitoredJob = selectedJob;
    const monitoredTarget = selectedTargetIndex;
    const request = controlRequestRef.current + 1;
    controlRequestRef.current = request;
    ticketRef.current = null;
    controlPendingRef.current = 'start';
    setControlPending('start');
    setWorkspaceStatus(`Starting AutoScan for '${monitoredJob.name}'…`);
    void ipc.startAutoScan(
      monitoredJob.job_id,
      monitoredJob.config_revision,
      monitoredTarget,
    ).then((next) => {
      if (controlRequestRef.current !== request) return;
      if (acceptStatus(next, 'start')) {
        setWorkspaceStatus(`AutoScan: ${next.detail}${next.auto_apply ? ' · unattended apply requires an exact prior grant' : ''}`);
      }
    }).catch((error) => {
      if (controlRequestRef.current !== request) return;
      setWorkspaceStatus(`AutoScan could not start: ${error}`, 'err');
    }).finally(() => {
      if (controlRequestRef.current === request) {
        controlPendingRef.current = null;
        setControlPending(null);
      }
    });
  };

  return { acceptStatus, toggle };
}
