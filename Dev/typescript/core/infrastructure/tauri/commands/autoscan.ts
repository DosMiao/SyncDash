import { invoke } from '@tauri-apps/api/core';

import {
  autoScanApplyAuthorizationArgs,
  startAutoScanArgs,
} from '#core/application/operations/operationProtocol.ts';
import type { AuthorizationDto } from '#core/types/generated/AuthorizationDto.ts';
import type { AutoScanStatusDto } from '#core/types/generated/AutoScanStatusDto.ts';

export const startAutoScan = (expectedJobId: string, expectedRevision: string, targetIndex: number) =>
  invoke<AutoScanStatusDto>('start_autoscan', startAutoScanArgs(expectedJobId, expectedRevision, targetIndex));

export const stopAutoScan = () => invoke<AutoScanStatusDto>('stop_autoscan');
export const autoScanStatus = () => invoke<AutoScanStatusDto>('autoscan_status');

export const declineAutoScanTrigger = (generation: number, ticketId: number) =>
  invoke<AutoScanStatusDto>('decline_autoscan_trigger', { generation, ticketId });

export const authorizeAutoScanApply = (generation: number, ticketId: number) =>
  invoke<AuthorizationDto>('authorize_autoscan_apply', autoScanApplyAuthorizationArgs(generation, ticketId));
