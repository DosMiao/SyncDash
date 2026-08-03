import type { Op } from '#core/types/generated/Op.ts';
import type { PlanDto as GeneratedPlanDto } from '#core/types/generated/PlanDto.ts';
import type { RowMeta } from '#core/types/generated/RowMeta.ts';
import type { ReviewedRowDecisionDto } from '#core/types/generated/ReviewedRowDecisionDto.ts';

export type PlanOperation = Op;
export type PlanDto = GeneratedPlanDto;

export const RESULT_TYPES = ['copy', 'update', 'move', 'delete', 'conflict', 'note'] as const;
export type ResultType = typeof RESULT_TYPES[number];
export interface ResultTypeDefinition {
  group: 'action' | 'report';
  label: string;
  pluralLabel: string;
}
export const RESULT_TYPE_DEFINITIONS: Record<ResultType, ResultTypeDefinition> = {
  copy: { group: 'action', label: 'Copy', pluralLabel: 'Copies' },
  update: { group: 'action', label: 'Update', pluralLabel: 'Updates' },
  move: { group: 'action', label: 'Move', pluralLabel: 'Moves' },
  delete: { group: 'action', label: 'Delete', pluralLabel: 'Deletes' },
  conflict: { group: 'report', label: 'Conflict', pluralLabel: 'Conflicts' },
  note: { group: 'report', label: 'Note', pluralLabel: 'Notes' },
};

/// Reports use `null` instead of an action direction.
export type ActionDirection = 'right' | 'left';

/// Side-specific keys preserve missing-side semantics in each displayed column.
export type SortKey = 's.path' | 't.path' | 'action' | 's.size' | 's.mtime' | 't.size' | 't.mtime' | 'reason';
export interface Sort { key: SortKey; dir: 1 | -1 }

export const SORT_LABEL: Record<SortKey, string> = {
  's.path': 'source path', 't.path': 'target path', action: 'action',
  's.size': 'source size', 's.mtime': 'source time',
  't.size': 'target size', 't.mtime': 'target time',
  reason: 'reason',
};

/// Same value as `pipeline::compare::policy::MTIME_SLACK_MS` on the Rust side: FAT/SMB
/// timestamp granularity. Presentation only — the engine's own comparison already applied it.
export const MTIME_SLACK_MS = 2000;

/// Every sentence that spells the tolerance out to the user derives it from the constant above, so
/// changing the engine value cannot leave prose claiming a tolerance the product no longer applies.
export const MTIME_SLACK_SECONDS = MTIME_SLACK_MS / 1000;

// Materialize and cache reversals only for requested rows; eager copies duplicate large plan graphs.
const reverseCache = new WeakMap<PlanOperation, PlanOperation | null>();

function reverseOperation(operation: PlanOperation): PlanOperation | null {
  if (reverseCache.has(operation)) return reverseCache.get(operation) ?? null;
  const side = operation.side === 'source' ? 'target' : 'source';
  const reason = `reversed(${operation.reason})`;
  let reversedOperation: PlanOperation | null;
  switch (operation.action) {
    case 'copy':
      reversedOperation = {
        ...operation, side, action: 'delete', from: null, mtime_ms: null,
        hash: null, link: null, reason,
      };
      break;
    case 'update':
      reversedOperation = {
        ...operation, side, from: null, size: null, mtime_ms: null, hash: null, reason,
      };
      break;
    case 'delete':
      reversedOperation = {
        ...operation, side, action: 'copy', from: null, mtime_ms: null, hash: null, reason,
      };
      break;
    default:
      reversedOperation = null;
  }
  reverseCache.set(operation, reversedOperation);
  return reversedOperation;
}

export function effectiveOperation(plan: PlanDto, rowReversed: boolean[], index: number): PlanOperation {
  const operation = plan.ops[index];
  return rowReversed[index] ? (reverseOperation(operation) ?? operation) : operation;
}

/// Apply sends decisions only; Rust reconstructs operations from the authenticated plan.
export function buildReviewedRowDecisions(
  indices: number[],
  rowReversed: boolean[],
): ReviewedRowDecisionDto[] {
  return indices.map((index) => ({
    index,
    direction_reversed: rowReversed[index] === true,
  }));
}

export function rowMetadata(plan: PlanDto, index: number): RowMeta {
  const metadata = plan.metas?.[index];
  if (metadata) return metadata;
  const operation = plan.ops[index];
  if (operation.action === 'copy' && operation.size != null && operation.mtime_ms != null) {
    const existingSide = { size: operation.size, mtime_ms: operation.mtime_ms };
    return operation.side === 'target'
      ? { src: existingSide, dst: null }
      : { src: null, dst: existingSide };
  }
  return { src: null, dst: null };
}

/// Result definitions are the single authority for execution eligibility.
export function isExecutableOperation(operation: PlanOperation): boolean {
  return RESULT_TYPE_DEFINITIONS[resultTypeOf(operation)].group === 'action';
}

export function canReverseOperation(plan: PlanDto, index: number): boolean {
  const action = plan.ops[index].action;
  return action === 'copy' || action === 'update' || action === 'delete';
}

export function resultTypeOf(operation: PlanOperation): ResultType {
  switch (operation.action) {
    case 'copy': return 'copy';
    case 'update': case 'chmod': return 'update';
    case 'move': return 'move';
    case 'delete': case 'delete_dir': return 'delete';
    case 'conflict': return 'conflict';
    case 'note': return 'note';
  }
}

/// Returns paths that exist at compare time, before executing the operation.
/// Copies and deletes have one present side; moves use `from` on the executing side.
export function sidePaths(operation: PlanOperation): [string | null, string | null] {
  const executesOnTarget = operation.side === 'target';
  switch (operation.action) {
    case 'copy':
      return executesOnTarget ? [operation.path, null] : [null, operation.path];
    case 'move': {
      const currentPath = operation.from ?? operation.path;
      return executesOnTarget
        ? [operation.path, currentPath]
        : [currentPath, operation.path];
    }
    case 'delete':
    case 'delete_dir':
      return executesOnTarget ? [null, operation.path] : [operation.path, null];
    default:
      return [operation.path, operation.path];
  }
}

/// Preserve engine verbs when several actions share one result facet.
const ACTION_LABEL: Record<PlanOperation['action'], string> = {
  copy: 'copy', update: 'update', chmod: 'chmod', move: 'move',
  delete: 'delete', delete_dir: 'delete', conflict: 'conflict', note: 'note',
};

export function describeRowAction(operation: PlanOperation): { direction: ActionDirection | null; resultType: ResultType; label: string } {
  return {
    direction: isExecutableOperation(operation) ? (operation.side === 'target' ? 'right' : 'left') : null,
    resultType: resultTypeOf(operation),
    label: ACTION_LABEL[operation.action],
  };
}

/// Must match the action rank in `Dev/src/workflow/pipeline/compare/planning/mod.rs`; the wire plan does not carry this rank.
function actionRank(operation: PlanOperation): number {
  switch (operation.action) {
    case 'move': return 0;
    case 'copy': case 'update': return 1;
    case 'chmod': return 2;
    case 'delete': return 3;
    case 'delete_dir': return 4;
    default: return 5;
  }
}

/// Folder folds are direction-independent so reversing sort direction cannot change aggregates.
export interface KeySpec {
  kind: 'num' | 'text';
  /// Directory keys use the group path; other keys aggregate member values.
  fold: 'dir' | 'min' | 'max' | 'sum';
  natural: 1 | -1;
}
export function keySpec(key: SortKey): KeySpec {
  switch (key) {
    // Member paths share the group prefix, so path columns sort by the group path itself.
    case 's.path': case 't.path': return { kind: 'text', fold: 'dir', natural: 1 };
    // The lowest action rank preserves the engine's grouped action order.
    case 'action': return { kind: 'num', fold: 'min', natural: 1 };
    // Sort by the same byte sum displayed on the group row.
    case 's.size': case 't.size': return { kind: 'num', fold: 'sum', natural: -1 };
    case 's.mtime': case 't.mtime': return { kind: 'num', fold: 'max', natural: -1 };
    case 'reason': return { kind: 'text', fold: 'min', natural: 1 };
  }
}

/// Missing values sort last in either direction; numeric keys remain numeric.
export function sortValue(plan: PlanDto, rowReversed: boolean[], index: number, key: SortKey): [number, number, string] {
  const operation = effectiveOperation(plan, rowReversed, index);
  const metadata = rowMetadata(plan, index);
  switch (key) {
    // Side paths preserve move origins and destinations independently.
    case 's.path': {
      const sourcePath = sidePaths(operation)[0];
      return [sourcePath ? 0 : 1, 0, sourcePath ? sourcePath.toLowerCase() : ''];
    }
    case 't.path': {
      const targetPath = sidePaths(operation)[1];
      return [targetPath ? 0 : 1, 0, targetPath ? targetPath.toLowerCase() : ''];
    }
    case 'action': return [0, actionRank(operation), ''];
    case 's.size': return [metadata.src ? 0 : 1, metadata.src?.size ?? 0, ''];
    case 's.mtime': return [metadata.src ? 0 : 1, metadata.src?.mtime_ms ?? 0, ''];
    case 't.size': return [metadata.dst ? 0 : 1, metadata.dst?.size ?? 0, ''];
    case 't.mtime': return [metadata.dst ? 0 : 1, metadata.dst?.mtime_ms ?? 0, ''];
    case 'reason': return [0, 0, operation.reason.toLowerCase()];
  }
}

export function rowSize(plan: PlanDto, rowReversed: boolean[], index: number): number {
  const metadata = rowMetadata(plan, index);
  return Math.max(
    effectiveOperation(plan, rowReversed, index).size ?? 0,
    metadata.src?.size ?? 0,
    metadata.dst?.size ?? 0,
  );
}

/// Reversed updates clear `Op.size`; transfer bytes therefore come from immutable per-side evidence.
export function rowTransferBytes(plan: PlanDto, rowReversed: boolean[], index: number): number {
  const operation = effectiveOperation(plan, rowReversed, index);
  if (operation.action !== 'copy' && operation.action !== 'update') return 0;
  const metadata = rowMetadata(plan, index);
  const origin = operation.side === 'target' ? metadata.src : metadata.dst;
  return Math.max(0, origin?.size ?? operation.size ?? 0);
}

export function rowModifiedTime(plan: PlanDto, index: number): number {
  const metadata = rowMetadata(plan, index);
  return Math.max(metadata.src?.mtime_ms ?? 0, metadata.dst?.mtime_ms ?? 0);
}

/// Returns no side when timestamps differ by at most `MTIME_SLACK_MS`.
export function newerSide(plan: PlanDto, index: number): '' | 's' | 't' {
  const metadata = rowMetadata(plan, index);
  if (!metadata.src || !metadata.dst) return '';
  if (metadata.src.mtime_ms - metadata.dst.mtime_ms > MTIME_SLACK_MS) return 's';
  if (metadata.dst.mtime_ms - metadata.src.mtime_ms > MTIME_SLACK_MS) return 't';
  return '';
}
