import type { AutoScanCompareRequestDto } from './types/generated/AutoScanCompareRequestDto';
import type { CompareIdentity } from './types/generated/CompareIdentity';
import type { OperationApprovalDto } from './types/generated/OperationApprovalDto';
import type { SelectedRowDto } from './types/generated/SelectedRowDto';

export function startAutoScanArgs(expectedJobId: string, expectedRevision: string, targetIndex: number) {
  return { expectedJobId, expectedRevision, targetIndex };
}

export function reviewCompareArgs(
  expectedJobId: string,
  targetIndex?: number,
  autoScanRequest?: AutoScanCompareRequestDto,
) {
  return {
    expectedJobId,
    ...(targetIndex === undefined ? {} : { targetIndex }),
    ...(autoScanRequest === undefined ? {} : { autoScanRequest }),
  };
}

export function approveOperationArgs(challengeId: string, approval: OperationApprovalDto) {
  return { challengeId, approval };
}

export function compareAuthorizationArgs(authorizationToken: string) {
  return { authorizationToken };
}

export function reviewApplyArgs(compareIdentity: CompareIdentity, selected: SelectedRowDto[]) {
  return { compareIdentity, selected };
}

export function autoScanApplyAuthorizationArgs(generation: number, ticketId: number) {
  return { generation, ticketId };
}

export function applyAuthorizationArgs(authorizationToken: string, launchId?: number) {
  return launchId === undefined
    ? { authorizationToken }
    : { authorizationToken, launchId };
}
