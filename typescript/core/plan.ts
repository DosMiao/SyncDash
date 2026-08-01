import type { Op } from './types/generated/Op';
import type { PlanDto as GeneratedPlanDto } from './types/generated/PlanDto';
import type { RowMeta } from './types/generated/RowMeta';
import type { SelectedRowDto } from './types/generated/SelectedRowDto';

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

/// The side on which an executable operation runs. Reports carry `null` at the call site.
export type ActionDirection = 'right' | 'left';

/// One key per sortable column. The two path keys are per-side rather than one shared 'path': the
/// table shows the source and the target path in separate columns, and a row missing that side has
/// nothing to sort by there — same rule the size and mtime keys already follow.
export type SortKey = 's.path' | 't.path' | 'action' | 's.size' | 's.mtime' | 't.size' | 't.mtime' | 'reason';
export interface Sort { key: SortKey; dir: 1 | -1 }

/// Unambiguous name for a sort key, for the "Sorted: …" indicator. The table's own headers say
/// `size` twice and `time` twice — which is fine in a column, and useless on its own.
export const SORT_LABEL: Record<SortKey, string> = {
  's.path': 'source path', 't.path': 'target path', action: 'action',
  's.size': 'source size', 's.mtime': 'source time',
  't.size': 'target size', 't.mtime': 'target time',
  reason: 'reason',
};

/// Same value as `MTIME_SLACK_MS` on the Rust side: FAT/SMB timestamp granularity.
export const MTIME_SLACK_MS = 2000;

// A 250k-row comparison used to ship a second complete Op for every reversible row. Besides
// duplicating paths and reasons across the Tauri JSON boundary, WebKit had to retain both object
// graphs and peaked above 1 GB before the table first painted. Reversal is needed only for rows the
// user explicitly flips, so derive and cache those few objects lazily from the original operation.
const reverseCache = new WeakMap<PlanOperation, PlanOperation | null>();

function reverseOperation(operation: PlanOperation): PlanOperation | null {
  if (reverseCache.has(operation)) return reverseCache.get(operation) ?? null;
  const side = operation.side === 'source' ? 'target' : 'source';
  const reason = `flipped(${operation.reason})`;
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

/// Returns the original or user-reversed operation currently represented by a row.
export function effectiveOperation(plan: PlanDto, flipped: boolean[], index: number): PlanOperation {
  const operation = plan.ops[index];
  return flipped[index] ? (reverseOperation(operation) ?? operation) : operation;
}

/// The apply boundary carries decisions, not operations. Rust reconstructs each row from the
/// authenticated plan and owns the executable reversal semantics.
export function selectedRows(indices: number[], flipped: boolean[]): SelectedRowDto[] {
  return indices.map((index) => ({ index, flipped: flipped[index] === true }));
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

/// Execution eligibility comes from the exhaustive result vocabulary, so facets and Apply cannot
/// acquire independent definitions of which rows are reports.
export function isExecutableOperation(operation: PlanOperation): boolean {
  return RESULT_TYPE_DEFINITIONS[resultTypeOf(operation)].group === 'action';
}

export function canReverseOperation(plan: PlanDto, index: number): boolean {
  const action = plan.ops[index].action;
  return action === 'copy' || action === 'update' || action === 'delete';
}

/// Engine-level chmod and directory deletion retain their precise row labels while joining the
/// broader Update and Delete result types respectively.
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

/// The paths that **currently exist** on the source / target side for this row (state at compare time, not after the run):
/// copy exists only on the origin side; delete only on the side being deleted; for move the executing side is still `from` while the other is already `path`;
/// update/chmod/conflict/note exist on both sides.
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

/// The verb a row states. Separate from `resultTypeOf()` because two actions share a type but not a
/// verb: chmod is an update and delete_dir is a delete, and both should still say what they are.
const ACTION_LABEL: Record<PlanOperation['action'], string> = {
  copy: 'copy', update: 'update', chmod: 'chmod', move: 'move',
  delete: 'delete', delete_dir: 'delete', conflict: 'conflict', note: 'note',
};

/// Reports have no execution direction; `isExecutableOperation()` reads that classification from
/// `RESULT_TYPE_DEFINITIONS`.
export function describeRowAction(operation: PlanOperation): { direction: ActionDirection | null; resultType: ResultType; label: string } {
  return {
    direction: isExecutableOperation(operation) ? (operation.side === 'target' ? 'right' : 'left') : null,
    resultType: resultTypeOf(operation),
    label: ACTION_LABEL[operation.action],
  };
}

/// Semantic order of the actions, mirroring the engine's own plan ordering in
/// `src/pipeline/compare.rs` (fn `compare`, the `rank` closure). Sorting the action column on the
/// serde string instead would order it `chmod < conflict < copy < delete < delete_dir < move`,
/// which is alphabetical trivia, not a ladder anyone means. If the Rust ranks change, this drifts —
/// that is the cost of a column the engine does not send us.
export function actionRank(operation: PlanOperation): number {
  switch (operation.action) {
    case 'move': return 0;
    case 'copy': case 'update': return 1;
    case 'chmod': return 2;
    case 'delete': return 3;
    case 'delete_dir': return 4;
    default: return 5;                       // conflict, note
  }
}

/// Everything about a sort key that is not "what is this row's value": whether it compares as a
/// number or as text, how a directory group folds its members' values into one, and which direction
/// a first click means. One exhaustive switch, so adding a key is a single edit the compiler forces
/// you to finish — the alternative is what this replaced, a `key === 'path' || key === 'action'`
/// literal check in the UI that a new key would silently fall through.
///
/// Folds are deliberately direction-independent. A group's aggregate has to be a property of the
/// group, not of which way you last clicked, or "why is this folder above that one" stops having an
/// answer you can check.
export interface KeySpec {
  kind: 'num' | 'text';
  /// dir = the group's own directory name; the others fold over member values
  fold: 'dir' | 'min' | 'max' | 'sum';
  natural: 1 | -1;
}
export function keySpec(key: SortKey): KeySpec {
  switch (key) {
    // A path column's group value is the directory itself. Folding member paths would be circular
    // (they all start with it) and would let one missing side sink a whole directory.
    case 's.path': case 't.path': return { kind: 'text', fold: 'dir', natural: 1 };
    // Lowest rank present: a folder of 500 copies and one note is a copy folder. This also keeps
    // ascending-by-action + grouped in the same group order as the unsorted grouped view.
    case 'action': return { kind: 'num', fold: 'min', natural: 1 };
    // Sizes are the one key with an additive meaning, and the group row already prints a byte sum —
    // ordering by a max while displaying a sum would leave the visible number column unordered.
    case 's.size': case 't.size': return { kind: 'num', fold: 'sum', natural: -1 };
    // "When was this folder last touched" is the only aggregate anyone means by a folder's time.
    case 's.mtime': case 't.mtime': return { kind: 'num', fold: 'max', natural: -1 };
    // Reasons are enumerable tags, not magnitudes; the alphabetically first is a stable label.
    case 'reason': return { kind: 'text', fold: 'min', natural: 1 };
  }
}

/// Sort value [missing flag, numeric key, text key]. A missing side always sorts last (regardless of
/// direction) so "things that aren't there" don't steal attention. Numeric keys compare as numbers — the
/// old code zero-padded size/mtime to 20-char strings, two allocations per comparator call, i.e. hundreds
/// of thousands of them for one pass over a few thousand rows
export function sortValue(plan: PlanDto, flipped: boolean[], index: number, key: SortKey): [number, number, string] {
  const operation = effectiveOperation(plan, flipped, index);
  const metadata = rowMetadata(plan, index);
  switch (key) {
    // Per-side paths, not operation.path: the column shows what sidePaths() put in it, and for a move that
    // is the origin on one side and the destination on the other
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

/// Largest size represented by either side of the compare row.
export function rowSize(plan: PlanDto, flipped: boolean[], index: number): number {
  const metadata = rowMetadata(plan, index);
  return Math.max(
    effectiveOperation(plan, flipped, index).size ?? 0,
    metadata.src?.size ?? 0,
    metadata.dst?.size ?? 0,
  );
}

/// Bytes that actually cross from the winning side for a copy/update. A reversed update deliberately
/// clears Op.size because its original evidence belongs to the other direction, so consult the
/// immutable per-side compare metadata according to the effective destination instead.
export function rowTransferBytes(plan: PlanDto, flipped: boolean[], index: number): number {
  const operation = effectiveOperation(plan, flipped, index);
  if (operation.action !== 'copy' && operation.action !== 'update') return 0;
  const metadata = rowMetadata(plan, index);
  const origin = operation.side === 'target' ? metadata.src : metadata.dst;
  return Math.max(0, origin?.size ?? operation.size ?? 0);
}

export function rowModifiedTime(plan: PlanDto, index: number): number {
  const metadata = rowMetadata(plan, index);
  return Math.max(metadata.src?.mtime_ms ?? 0, metadata.dst?.mtime_ms ?? 0);
}

/// Which side is meaningfully newer, for the tinted meta column ('' = within the slack, i.e. the same age)
export function newerSide(plan: PlanDto, index: number): '' | 's' | 't' {
  const metadata = rowMetadata(plan, index);
  if (!metadata.src || !metadata.dst) return '';
  if (metadata.src.mtime_ms - metadata.dst.mtime_ms > MTIME_SLACK_MS) return 's';
  if (metadata.dst.mtime_ms - metadata.src.mtime_ms > MTIME_SLACK_MS) return 't';
  return '';
}
