import type { CompareOwner } from './types/generated/CompareOwner';
import type { SelectedRowDto } from './types/generated/SelectedRowDto';

export function startAutoScanArgs(expectedJobId: string, expectedRevision: string, targetIndex: number) {
  return { expectedJobId, expectedRevision, targetIndex };
}

export function reviewCompareArgs(expectedJobId: string, targetIndex?: number) {
  return targetIndex === undefined
    ? { expectedJobId }
    : { expectedJobId, targetIndex };
}

export function approveOperationArgs(
  challengeId: string,
  acknowledgeHealth: boolean,
  acceptCapabilities: boolean,
  rememberForSession: boolean,
  allowUnattended: boolean,
) {
  return {
    challengeId,
    acknowledgeHealth,
    acceptCapabilities,
    rememberForSession,
    allowUnattended,
  };
}

export function compareAuthorizationArgs(authorizationToken: string) {
  return { authorizationToken };
}

export function reviewApplyArgs(owner: CompareOwner, selected: SelectedRowDto[]) {
  return { owner, selected };
}

export function autoScanApplyAuthorizationArgs(generation: number, ticketId: number) {
  return { generation, ticketId };
}

export function applyAuthorizationArgs(authorizationToken: string, launchId?: number) {
  return launchId === undefined
    ? { authorizationToken }
    : { authorizationToken, launchId };
}
