import type { PlanLayout } from '#core/domain/compare/grouping.ts';
import type { PlanDto, ResultType } from '#core/domain/compare/plan.ts';
import { compareScopeForJob } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { CompareScopeKey, CompareResultKey } from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { RootEditorKey, RootEditorOwner, RootValues } from '#core/application/jobs/rootEditor.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { JobRootMutationDto } from '#core/types/generated/JobRootMutationDto.ts';

/// Stable empty identities keep memoized presentation derivations from rebuilding on every render.
export const EMPTY_LAYOUT: PlanLayout = { displayOrder: [], folderTree: null };
export const EMPTY_FLAGS: boolean[] = [];
export const EMPTY_RESULT_TYPES = new Set<ResultType>();
export const EMPTY_PATH_SET = new Set<string>();

/// Built when a row menu opens so every action owns the exact row and plan snapshot it describes.
export interface ContextMenuEntry {
  label: string;
  disabled?: boolean;
  danger?: boolean;
  separator?: boolean;
  run?: () => void;
}

export interface ContextMenuState {
  x: number;
  y: number;
  entries: ContextMenuEntry[];
}

export interface CompareCompletion { plan: PlanDto }
export interface JobIdentitySnapshot { jobId: string; name: string; configRevision: string }
export interface CompareActivityRequest {
  scope: ReturnType<typeof compareScopeForJob>;
  requestId: number;
}
export interface RootSwapRequest {
  workspaceKey: RootEditorKey;
  owner: RootEditorOwner;
  values: RootValues;
  mode: string;
}
export interface CandidateAdoption {
  scopeKey: CompareScopeKey;
  resultKey: CompareResultKey;
}

export function snapshotJobIdentity(job: JobDto): JobIdentitySnapshot {
  return { jobId: job.job_id, name: job.name, configRevision: job.config_revision };
}

export function rootMutationState(
  result: JobRootMutationDto,
  targetIndex: number,
): { owner: RootEditorOwner; values: RootValues } {
  const target = result.targets[targetIndex];
  if (target === undefined) {
    throw new Error(`The root-mutation response omitted target ${targetIndex + 1}`);
  }
  return {
    owner: {
      jobId: result.mutation.job_id,
      jobName: result.mutation.name,
      configRevision: result.mutation.config_revision,
      targetIndex,
    },
    values: { source: result.source, target },
  };
}

export function statusDeliveryWarning(mutation: { status_delivery_warnings: string[] }): string {
  return mutation.status_delivery_warnings.length
    ? ` · desktop status delivery warning: ${mutation.status_delivery_warnings.join('; ')}`
    : '';
}
