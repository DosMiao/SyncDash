// Row semantics of a compare result: which op a row currently carries, which side it touches, how it
// is categorized, labelled and sorted. Every one of these used to read a module-global `plan`/`flipped`
// in main.ts; they now take the state explicitly so a React render can call them from anywhere.

import type { Op } from './types/generated/Op';
import type { CompareOwner } from './types/generated/CompareOwner';
import type { PlanHeader } from './types/generated/PlanHeader';
import type { RowMeta } from './types/generated/RowMeta';
import type { SelectedRowDto } from './types/generated/SelectedRowDto';

export type OpDto = Op;

/// Return value of `compare_job`. The Rust side generates a DTO of the same name (PlanDto.ts),
/// but there `header` is a `PlanHeader` reference; keeping the shape identical here is enough.
export interface PlanDto {
  owner: CompareOwner;
  header: PlanHeader;
  ops: OpDto[];
  /// Measured size/mtime for both sides. Copy rows are null because Op already carries their sole side.
  metas: (RowMeta | null)[];
  equal_count: number;
  equal_bytes: number;
}

/// The six things a plan row can be — the whole vocabulary the UI colours and draws. `Chip` is this
/// set minus `note` (which category() folds into conflict) plus an `all` pseudo-entry, so the two
/// are deliberately separate names derived from one list rather than two hand-kept unions.
export type Kind = 'copy' | 'update' | 'move' | 'delete' | 'conflict' | 'note';
export type Chip = 'all' | Exclude<Kind, 'note'>;
export const CHIPS: [Chip, string][] = [
  ['all', 'All'], ['copy', 'Copy'], ['update', 'Update'], ['move', 'Move'], ['delete', 'Delete'], ['conflict', 'Conflict/Note'],
];

/// The direction the bytes move. Null nowhere in this type — a row that has no direction (a report)
/// carries `null` at the site that asks for one.
export type Dir = 'right' | 'left';

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

/// Same value as MTIME_SLACK_MS on the Rust side: FAT/SMB timestamp granularity — under 2 s is not an "update"
export const MTIME_SLACK = 2000;

// A 250k-row comparison used to ship a second complete Op for every reversible row. Besides
// duplicating paths and reasons across the Tauri JSON boundary, WebKit had to retain both object
// graphs and peaked above 1 GB before the table first painted. Reversal is needed only for rows the
// user explicitly flips, so derive and cache those few objects lazily from the original operation.
const reverseCache = new WeakMap<OpDto, OpDto | null>();

function reverseOp(op: OpDto): OpDto | null {
  if (reverseCache.has(op)) return reverseCache.get(op) ?? null;
  const side = op.side === 'source' ? 'target' : 'source';
  const reason = `flipped(${op.reason})`;
  let out: OpDto | null;
  switch (op.action) {
    case 'copy':
      out = {
        ...op, side, action: 'delete', from: null, mtime_ms: null,
        hash: null, link: null, reason,
      };
      break;
    case 'update':
      out = {
        ...op, side, from: null, size: null, mtime_ms: null, hash: null, reason,
      };
      break;
    case 'delete':
      out = { ...op, side, action: 'copy', from: null, mtime_ms: null, reason };
      break;
    default:
      out = null;
  }
  reverseCache.set(op, out);
  return out;
}

/// The op currently in effect for this row (once flipped, the reversed one)
export function eff(plan: PlanDto, flipped: boolean[], i: number): OpDto {
  const op = plan.ops[i];
  return flipped[i] ? (reverseOp(op) ?? op) : op;
}

/// The apply boundary carries decisions, not operations. Rust reconstructs each row from the
/// authenticated plan and owns the executable reversal semantics.
export function selectedRows(indices: number[], flipped: boolean[]): SelectedRowDto[] {
  return indices.map((index) => ({ index, flipped: flipped[index] === true }));
}

export function metaOf(plan: PlanDto, i: number): RowMeta {
  const sent = plan.metas?.[i];
  if (sent) return sent;
  const op = plan.ops[i];
  if (op.action === 'copy' && op.size != null && op.mtime_ms != null) {
    const one = { size: op.size, mtime_ms: op.mtime_ms };
    return op.side === 'target' ? { src: one, dst: null } : { src: null, dst: one };
  }
  return { src: null, dst: null };
}

/// Conflicts and notes are reports, not actions: they can neither be checked nor reversed
export function selectable(op: OpDto): boolean {
  return op.action !== 'conflict' && op.action !== 'note';
}

export function canFlip(plan: PlanDto, i: number): boolean {
  const action = plan.ops[i].action;
  return action === 'copy' || action === 'update' || action === 'delete';
}

/// Never returns 'all' — that chip is the *absence* of a filter, not a category — so the return type
/// says so, and rowAction() below can use it without having to rule the pseudo-entry back out.
export function category(op: OpDto): Exclude<Chip, 'all'> {
  switch (op.action) {
    case 'copy': return 'copy';
    case 'update': case 'chmod': return 'update';
    case 'move': return 'move';
    case 'delete': case 'delete_dir': return 'delete';
    default: return 'conflict';
  }
}

/// The paths that **currently exist** on the source / target side for this row (state at compare time, not after the run):
/// copy exists only on the origin side; delete only on the side being deleted; for move the executing side is still `from` while the other is already `path`;
/// update/chmod/conflict/note exist on both sides.
export function sidePaths(op: OpDto): [string | null, string | null] {
  const execOnTarget = op.side === 'target';
  switch (op.action) {
    case 'copy':
      return execOnTarget ? [op.path, null] : [null, op.path];
    case 'move': {
      const cur = op.from ?? op.path;
      return execOnTarget ? [op.path, cur] : [cur, op.path];
    }
    case 'delete':
    case 'delete_dir':
      return execOnTarget ? [null, op.path] : [op.path, null];
    default:
      return [op.path, op.path];
  }
}

/// The verb a row states. Separate from `category()` because two actions share a category but not a
/// verb: chmod is an update and delete_dir is a delete, and both should still say what they are.
const LABEL: Record<OpDto['action'], string> = {
  copy: 'copy', update: 'update', chmod: 'chmod', move: 'move',
  delete: 'delete', delete_dir: 'delete', conflict: 'conflict', note: 'note',
};

/// What the action cell states: the direction the bytes move, the category its glyph and colour come
/// from, and the verb. The cell composes them as [dir][kind] label.
///
/// `dir` is null exactly for the two reports — "is a report" is already what selectable() decides,
/// so there is one definition of it rather than two lists that can drift.
export function rowAction(op: OpDto): { dir: Dir | null; kind: Kind; label: string } {
  return {
    dir: selectable(op) ? (op.side === 'target' ? 'right' : 'left') : null,
    kind: op.action === 'note' ? 'note' : category(op),
    label: LABEL[op.action],
  };
}

/// Semantic order of the actions, mirroring the engine's own plan ordering in
/// `src/pipeline/compare.rs` (fn `compare`, the `rank` closure). Sorting the action column on the
/// serde string instead would order it `chmod < conflict < copy < delete < delete_dir < move`,
/// which is alphabetical trivia, not a ladder anyone means. If the Rust ranks change, this drifts —
/// that is the cost of a column the engine does not send us.
export function actionRank(op: OpDto): number {
  switch (op.action) {
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
export function sortVal(plan: PlanDto, flipped: boolean[], i: number, key: SortKey): [number, number, string] {
  const op = eff(plan, flipped, i);
  const m = metaOf(plan, i);
  switch (key) {
    // Per-side paths, not op.path: the column shows what sidePaths() put in it, and for a move that
    // is the origin on one side and the destination on the other
    case 's.path': { const p = sidePaths(op)[0]; return [p ? 0 : 1, 0, p ? p.toLowerCase() : '']; }
    case 't.path': { const p = sidePaths(op)[1]; return [p ? 0 : 1, 0, p ? p.toLowerCase() : '']; }
    case 'action': return [0, actionRank(op), ''];
    case 's.size': return [m.src ? 0 : 1, m.src?.size ?? 0, ''];
    case 's.mtime': return [m.src ? 0 : 1, m.src?.mtime_ms ?? 0, ''];
    case 't.size': return [m.dst ? 0 : 1, m.dst?.size ?? 0, ''];
    case 't.mtime': return [m.dst ? 0 : 1, m.dst?.mtime_ms ?? 0, ''];
    case 'reason': return [0, 0, op.reason.toLowerCase()];
  }
}

/// Bytes and time this row involves: take the larger/newer of the two sides ("how big and how new is the file behind this row")
export function rowSize(plan: PlanDto, flipped: boolean[], i: number): number {
  const m = metaOf(plan, i);
  return Math.max(eff(plan, flipped, i).size ?? 0, m.src?.size ?? 0, m.dst?.size ?? 0);
}

export function rowMtime(plan: PlanDto, i: number): number {
  const m = metaOf(plan, i);
  return Math.max(m.src?.mtime_ms ?? 0, m.dst?.mtime_ms ?? 0);
}

/// Which side is meaningfully newer, for the tinted meta column ('' = within the slack, i.e. the same age)
export function newerSide(plan: PlanDto, i: number): '' | 's' | 't' {
  const m = metaOf(plan, i);
  if (!m.src || !m.dst) return '';
  if (m.src.mtime_ms - m.dst.mtime_ms > MTIME_SLACK) return 's';
  if (m.dst.mtime_ms - m.src.mtime_ms > MTIME_SLACK) return 't';
  return '';
}
