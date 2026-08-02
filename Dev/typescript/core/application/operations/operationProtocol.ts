import type { AutoScanCompareRequestDto } from '#core/types/generated/AutoScanCompareRequestDto.ts';
import type { CompareIdentity } from '#core/types/generated/CompareIdentity.ts';
import type { OperationApprovalDto } from '#core/types/generated/OperationApprovalDto.ts';
import type { ReviewedRowDecisionDto } from '#core/types/generated/ReviewedRowDecisionDto.ts';

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

export function reviewApplyArgs(
  compareIdentity: CompareIdentity,
  reviewedRowDecisions: ReviewedRowDecisionDto[],
) {
  return { compareIdentity, reviewedRowDecisions };
}

export function autoScanApplyAuthorizationArgs(generation: number, ticketId: number) {
  return { generation, ticketId };
}

export function applyAuthorizationArgs(authorizationToken: string, launchId?: number) {
  return launchId === undefined
    ? { authorizationToken }
    : { authorizationToken, launchId };
}
