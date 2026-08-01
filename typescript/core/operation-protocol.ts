import type { CompareOwner } from './types/generated/CompareOwner';
import type { SelectedRowDto } from './types/generated/SelectedRowDto';

export function reviewCompareArgs(name: string, expectedJobId: string, targetIndex?: number) {
  return targetIndex === undefined
    ? { name, expectedJobId }
    : { name, expectedJobId, targetIndex };
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

export function unattendedApplyAuthorizationArgs(owner: CompareOwner, selected: SelectedRowDto[]) {
  return { owner, selected };
}

export function applyAuthorizationArgs(authorizationToken: string, launchId?: number) {
  return launchId === undefined
    ? { authorizationToken }
    : { authorizationToken, launchId };
}
