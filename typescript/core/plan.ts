// Row semantics of a compare result: which op a row currently carries, which side it touches, how it
// is categorized, labelled and sorted. Every one of these used to read a module-global `plan`/`flipped`
// in main.ts; they now take the state explicitly so a React render can call them from anywhere.

import type { Op } from './types/generated/Op';
import type { PlanHeader } from './types/generated/PlanHeader';
import type { RowMeta } from './types/generated/RowMeta';

export type OpDto = Op;

/// Return value of `compare_job`. The Rust side generates a DTO of the same name (PlanDto.ts),
/// but there `header` is a `PlanHeader` reference; keeping the shape identical here is enough.
export interface PlanDto {
  header: PlanHeader;
  ops: OpDto[];
  reversed: (OpDto | null)[];
  /// Measured size/mtime for both sides, one entry per op (produced by compare::evidence on the Rust side)
  metas: RowMeta[];
  equal_count: number;
  equal_bytes: number;
}

export type Chip = 'all' | 'copy' | 'update' | 'move' | 'delete' | 'conflict';
export const CHIPS: [Chip, string][] = [
  ['all', 'All'], ['copy', 'Copy'], ['update', 'Update'], ['move', 'Move'], ['delete', 'Delete'], ['conflict', 'Conflict/Note'],
];

export type SortKey = 'path' | 'action' | 's.size' | 's.mtime' | 't.size' | 't.mtime';
export interface Sort { key: SortKey; dir: 1 | -1 }

/// Same value as MTIME_SLACK_MS on the Rust side: FAT/SMB timestamp granularity — under 2 s is not an "update"
export const MTIME_SLACK = 2000;

/// The op currently in effect for this row (once flipped, the reversed one)
export function eff(plan: PlanDto, flipped: boolean[], i: number): OpDto {
  return flipped[i] && plan.reversed[i] ? plan.reversed[i]! : plan.ops[i];
}

export function metaOf(plan: PlanDto, i: number): RowMeta {
  return plan.metas?.[i] ?? { src: null, dst: null };
}

/// Conflicts and notes are reports, not actions: they can neither be checked nor reversed
export function selectable(op: OpDto): boolean {
  return op.action !== 'conflict' && op.action !== 'note';
}

export function canFlip(plan: PlanDto, i: number): boolean {
  return !!plan.reversed[i] && selectable(plan.ops[i]);
}

export function category(op: OpDto): Chip {
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

/// Badge text and color class. The arrow is the direction the bytes move, not which side is "source".
export function badge(op: OpDto): { text: string; cls: string } {
  const toTarget = op.side === 'target';
  switch (op.action) {
    case 'copy': return { text: toTarget ? '→ copy' : '← copy', cls: toTarget ? 'copy-r' : 'copy-l' };
    case 'update': return { text: toTarget ? '→ update' : '← update', cls: 'update' };
    case 'move': return { text: (toTarget ? '→' : '←') + ' move', cls: 'mv' };
    case 'delete':
    case 'delete_dir': return { text: (toTarget ? '→' : '←') + ' delete', cls: 'del' };
    case 'chmod': return { text: (toTarget ? '→' : '←') + ' chmod', cls: 'update' };
    case 'conflict': return { text: '⚡ conflict', cls: 'conflict' };
    default: return { text: 'ⓘ note', cls: 'note' };
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
    case 'path': return [0, 0, op.path.toLowerCase()];
    case 'action': return [0, 0, op.action];
    case 's.size': return [m.src ? 0 : 1, m.src?.size ?? 0, ''];
    case 's.mtime': return [m.src ? 0 : 1, m.src?.mtime_ms ?? 0, ''];
    case 't.size': return [m.dst ? 0 : 1, m.dst?.size ?? 0, ''];
    case 't.mtime': return [m.dst ? 0 : 1, m.dst?.mtime_ms ?? 0, ''];
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
