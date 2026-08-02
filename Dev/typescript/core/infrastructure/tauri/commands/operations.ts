import { invoke } from '@tauri-apps/api/core';

import {
  approveOperationArgs,
  reviewApplyArgs,
  reviewCompareArgs,
} from '#core/application/operations/operationProtocol.ts';
import type { AuthorizationDto } from '#core/types/generated/AuthorizationDto.ts';
import type { AutoScanCompareRequestDto } from '#core/types/generated/AutoScanCompareRequestDto.ts';
import type { CompareIdentity } from '#core/types/generated/CompareIdentity.ts';
import type { OperationApprovalDto } from '#core/types/generated/OperationApprovalDto.ts';
import type { OperationReviewDto } from '#core/types/generated/OperationReviewDto.ts';
import type { ReviewedRowDecisionDto } from '#core/types/generated/ReviewedRowDecisionDto.ts';

export type { AuthorizationDto, OperationReviewDto };

export const reviewCompare = (
  expectedJobId: string,
  targetIndex?: number,
  autoScanRequest?: AutoScanCompareRequestDto,
) => invoke<OperationReviewDto>(
  'review_compare',
  reviewCompareArgs(expectedJobId, targetIndex, autoScanRequest),
);

export const approveOperation = (challengeId: string, approval: OperationApprovalDto) =>
  invoke<AuthorizationDto>('approve_operation', approveOperationArgs(challengeId, approval));

export const reviewApply = (
  compareIdentity: CompareIdentity,
  reviewedRowDecisions: ReviewedRowDecisionDto[],
) => invoke<OperationReviewDto>(
  'review_apply',
  reviewApplyArgs(compareIdentity, reviewedRowDecisions),
);
