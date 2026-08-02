import { useRef, useState } from 'react';
import type { Dispatch, MutableRefObject, SetStateAction } from 'react';
import { AutoScanTicketLedger } from '#core/application/autoscan/autoscan.ts';
import type { AutoScanTicket } from '#core/application/autoscan/autoscan.ts';
import type { AutoScanStatusDto } from '#core/types/generated/AutoScanStatusDto.ts';
import type { AutoScanTriggerDto } from '#core/types/generated/AutoScanTriggerDto.ts';

export interface AutoScanRuntimeState {
  status: AutoScanStatusDto | null;
  setStatus: Dispatch<SetStateAction<AutoScanStatusDto | null>>;
  statusRef: MutableRefObject<AutoScanStatusDto | null>;
  ticketRef: MutableRefObject<AutoScanTicket | null>;
  ledgerRef: MutableRefObject<AutoScanTicketLedger>;
  controlRequestRef: MutableRefObject<number>;
  controlPending: 'start' | 'stop' | null;
  setControlPending: Dispatch<SetStateAction<'start' | 'stop' | null>>;
  controlPendingRef: MutableRefObject<'start' | 'stop' | null>;
  triggerRef: MutableRefObject<(trigger: AutoScanTriggerDto) => Promise<void>>;
}

export function useAutoScanState(): AutoScanRuntimeState {
  const [status, setStatus] = useState<AutoScanStatusDto | null>(null);
  const statusRef = useRef<AutoScanStatusDto | null>(null);
  const ticketRef = useRef<AutoScanTicket | null>(null);
  const ledgerRef = useRef(new AutoScanTicketLedger());
  const controlRequestRef = useRef(0);
  const [controlPending, setControlPending] = useState<'start' | 'stop' | null>(null);
  const controlPendingRef = useRef<'start' | 'stop' | null>(null);
  const triggerRef = useRef<(trigger: AutoScanTriggerDto) => Promise<void>>(async () => {});

  return {
    status,
    setStatus,
    statusRef,
    ticketRef,
    ledgerRef,
    controlRequestRef,
    controlPending,
    setControlPending,
    controlPendingRef,
    triggerRef,
  };
}
