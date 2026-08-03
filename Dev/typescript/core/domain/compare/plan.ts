import type { Op } from '#core/types/generated/Op.ts';
import type { PlanDto as GeneratedPlanDto } from '#core/types/generated/PlanDto.ts';
import type { RowMeta } from '#core/types/generated/RowMeta.ts';
import type { ReviewedRowDecisionDto } from '#core/types/generated/ReviewedRowDecisionDto.ts';

/// Four rules in this module are re-derivations of `syncdash::pipeline::compare::evidence`, which
/// owns all four. They are here because the window cannot ask the backend per click for a direction
/// toggle or per keystroke for a six-figure table, and they are held to the engine by
/// Rust-generated vectors — see `Script/tests/compare-plan-rules.test.mts`.
///
/// `reverseOperation` mirrors engine semantics: Apply sends `{index, direction_reversed}` and Rust
/// reconstructs the executed operation from the authenticated plan, so this copy only ever previews
/// and totals. `sidePaths` and `rowMetadata` are presentation and wire decoding respectively.
/// `actionRank` recovers the engine's own grouping so an action sort cannot invent a new one.
/// Execution membership stays with Run Scope in engine plan order; grouping, sorting, folding, and
/// path shortening are this layer's alone and have no counterpart in Rust.
///
/// The mtime equality window is not a fifth: it is a per-run measurement the result publishes as
/// `plan.mtime_window_ms`, because a run widens it to the coarser backend's declared precision.
/// `MTIME_WINDOW_FLOOR_MS` in the generated contracts states the configured floor and nothing more.

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

// Materialize and cache reversals only for requested rows; eager copies duplicate large plan graphs.
// Keyed by the operation because `metas[i]` is defined as one entry per op: a row's retained
// evidence cannot vary while the row does not.
const reverseCache = new WeakMap<PlanOperation, PlanOperation | null>();

/// Mirrors `pipeline::compare::evidence::reverse_op` arm for arm. Move, directory, conflict, and
/// note rows are not reversible, and neither is a content Update whose new origin side was never
/// measured: the reversed row writes the bytes that side was observed to hold, and a sizeless write
/// row is read as zero bytes by the free-space gate. Returning null is how both languages say so.
function reverseOperation(operation: PlanOperation, metadata: RowMeta): PlanOperation | null {
  if (reverseCache.has(operation)) return reverseCache.get(operation) ?? null;
  const side = operation.side === 'source' ? 'target' : 'source';
  const reason = `reversed(${operation.reason})`;
  let reversedOperation: PlanOperation | null;
  switch (operation.action) {
    case 'copy':
      // The content evidence describes a file about to be removed, not written. `size` stays: it is
      // what the deletion tally is measured in.
      reversedOperation = {
        ...operation, side, action: 'delete', from: null, mtime_ms: null,
        hash: null, link: null, reason,
      };
      break;
    case 'update': {
      // The other side's content wins, so the row no longer describes this side's bytes: the new
      // origin is the side that was about to be written. A symlink op publishes a link rather than
      // content and is measured on neither side, so it keeps its absent size.
      let size: number | null = null;
      if (operation.link == null) {
        const newOrigin = operation.side === 'source' ? metadata.src : metadata.dst;
        if (!newOrigin) {
          reversedOperation = null;
          break;
        }
        size = newOrigin.size;
      }
      reversedOperation = {
        ...operation, side, from: null, size, mtime_ms: null, hash: null, reason,
      };
      break;
    }
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

/// Throws where the engine refuses. Answering with the forward operation instead would offer the
/// operator the opposite direction from the one they asked for, and Rust rejects the row at Apply
/// regardless — `canReverseOperation` is the gate every caller has to pass first.
export function effectiveOperation(plan: PlanDto, rowReversed: boolean[], index: number): PlanOperation {
  const operation = plan.ops[index];
  if (rowReversed[index] !== true) return operation;
  const reversed = reverseOperation(operation, rowMetadata(plan, index));
  if (reversed === null) throw new Error(`Compare row ${index + 1} cannot be reversed`);
  return reversed;
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

/// Decodes the wire compression `pipeline::compare::evidence::implied_row_meta` defines: a `Copy`
/// row's sole side is already in the row, so its `metas` entry is elided and rebuilt here.
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

/// Asks the reversal itself rather than re-listing the actions, so the toggle a row offers and the
/// row the engine will accept can never be answered by two different rules.
export function canReverseOperation(plan: PlanDto, index: number): boolean {
  return reverseOperation(plan.ops[index], rowMetadata(plan, index)) !== null;
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
/// Mirrors `pipeline::compare::evidence::side_paths`, which the CSV export and File-Manager reveal
/// read. Shortening a rendered path is this layer's own and deliberately has no Rust counterpart.
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

/// Mirrors `Action::plan_rank`; the wire plan records the order it produced but not the rank itself.
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

/// The origin side's immutable evidence is the arbiter: a reversed row's own `size` was rebuilt
/// from that same measurement, and `Op.size` is only the fallback for a row that retained none.
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

/// Returns no side when the two timestamps are within the window *this* comparison applied.
///
/// The window is a per-run measurement, not a constant: `pipeline::compare::policy` supplies a
/// floor and `run::local::compare` widens it to the coarser of the two backends' declared mtime
/// precision, so on a minute-precision root the engine rules pairs equal that the floor would
/// separate. Reading `plan.mtime_window_ms` is what stops this cue from contradicting the verdict
/// the same result already carries.
export function newerSide(plan: PlanDto, index: number): '' | 's' | 't' {
  const metadata = rowMetadata(plan, index);
  if (!metadata.src || !metadata.dst) return '';
  if (metadata.src.mtime_ms - metadata.dst.mtime_ms > plan.mtime_window_ms) return 's';
  if (metadata.dst.mtime_ms - metadata.src.mtime_ms > plan.mtime_window_ms) return 't';
  return '';
}
