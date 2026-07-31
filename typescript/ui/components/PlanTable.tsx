import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { ChevronDown, ChevronRight, ChevronUp } from 'lucide-react';
import { baseOf, dirOf, fmtTime, fullPath, humanSize } from '../../core/format';
import { canFlip, eff, metaOf, newerSide, rowAction, selectable, sidePaths } from '../../core/plan';
import { DIR_ICON, MARK } from '../icons';
import { useVirtualRows } from '../hooks/useVirtualRows';
import type { ReactNode } from 'react';
import type { RowSpec } from '../../core/grouping';
import type { PlanDto, SortKey, Sort } from '../../core/plan';
import type { SideMeta } from '../../core/types/generated/SideMeta';

interface Props {
  plan: PlanDto;
  flipped: boolean[];
  checked: boolean[];
  rowPlan: RowSpec[];
  visible: number[];
  pathMode: 'rel' | 'full';
  grouped: boolean;
  sort: Sort | null;
  collapsedDirs: Set<string>;
  /// The scroll container, owned by whoever rendered this table. Both the virtual window and the
  /// column policy measure it, and neither can find it on its own without reaching into the DOM.
  wrap: HTMLElement | null;
  /// Changing this string scrolls the body back to the top — a filter change should not leave you
  /// staring at row 4000 of a list that no longer has one
  resetKey: string;
  onToggleRow: (i: number, value: boolean) => void;
  onToggleMany: (items: number[], value: boolean) => void;
  onFlip: (i: number) => void;
  onFoldDir: (dir: string) => void;
  onSort: (key: SortKey) => void;
  onContextRow: (i: number, x: number, y: number) => void;
}

/// Named for what each rung drops. They are cumulative: `nosize` has no time column either, having
/// already lost it one rung up.
type ColMode = 'full' | 'noreason' | 'notime' | 'nosize';

/// A column's identity **is** its sort key — there is no column you can sort by two ways, and no key
/// without a column. Only the checkbox has neither.
type ColId = 'chk' | SortKey;

interface ColDef {
  id: ColId;
  /// Keys this header takes over in the modes where their own column is gone. A column that drops
  /// must not take its sort key with it: the header that owns the key would then be unmounted in
  /// exactly the mode where it is the last place left to click, leaving a stale sort you can see but
  /// not change.
  adopts?: Partial<Record<ColMode, SortKey[]>>;
  /// Width per mode. **A mode absent from this map means the column is not rendered in it** —
  /// presence and width are one fact, so they cannot drift apart. null = flexible: only the two path
  /// columns, which get no <col> width and so split whatever the fixed ones leave, evenly.
  w: Partial<Record<ColMode, number | null>>;
  /// Class on both <th> and <td>. Never a width — <colgroup> owns those.
  cls: string;
  /// Header tooltip, for a column whose behaviour needs a sentence the label cannot carry
  headTitle?: string;
}

/// Header text. Short and contextual, because the column it sits over says which side it is;
/// SORT_LABEL in core/plan.ts carries the unambiguous name for the "sorted by" indicator.
const COL_HEAD: Record<SortKey, string> = {
  's.path': 'source', 't.path': 'target', action: 'action',
  's.size': 'size', 't.size': 'size', 's.mtime': 'time', 't.mtime': 'time',
  reason: 'reason',
};

/// The table's whole layout, in order. The row reads left to right as source facts → what happens →
/// target facts, so the action sits on the axis between the two sides rather than in front of them,
/// and each side's size and time are their own columns rather than one fused cell.
///
/// One descriptor per column drives the <colgroup>, the <thead> and the row cells. The alternative —
/// a width array per mode plus `mode === 'full' &&` guards repeated in the header and the body — is a
/// dozen places that have to agree, and under `table-layout: fixed` a disagreement is *silent*:
/// surplus <col>s are ignored and missing ones make the trailing columns split the remainder, so the
/// failure reads as "the widths went odd at one window size" and nobody bisects it.
///
/// Widths at each mode's own floor leave the two path columns 191 / 191 / 157 / 199 px — they are
/// the primary content and never get starved. The 1240→1000 step is exactly the reason column's
/// 240px, so crossing it costs the paths nothing.
const COLS: ColDef[] = [
  { id: 'chk', cls: 'c-chk', w: { full: 38, noreason: 38, notime: 38, nosize: 38 } },
  {
    id: 's.path', cls: 'c-path c-path-s',
    adopts: { nosize: ['s.size', 's.mtime'] },
    w: { full: null, noreason: null, notime: null, nosize: null },
  },
  // 112 rather than 92 in notime: an ellipsized "size · ti…" would leave the time span unclickable,
  // and thead clips. 92 fits "894.0 MB"; 112 fits the composite header above it.
  { id: 's.size', cls: 'c-size', adopts: { notime: ['s.mtime'] }, w: { full: 92, noreason: 92, notime: 112 } },
  { id: 's.mtime', cls: 'c-time', w: { full: 136, noreason: 136 } },
  {
    id: 'action', cls: 'c-act',
    w: { full: 124, noreason: 124, notime: 124, nosize: 124 },
    headTitle: "Sort by action. Click a row's action to reverse its direction; click again to restore.",
  },
  {
    id: 't.path', cls: 'c-path c-path-t',
    adopts: { nosize: ['t.size', 't.mtime'] },
    w: { full: null, noreason: null, notime: null, nosize: null },
  },
  { id: 't.size', cls: 'c-size', adopts: { notime: ['t.mtime'] }, w: { full: 92, noreason: 92, notime: 112 } },
  { id: 't.mtime', cls: 'c-time', w: { full: 136, noreason: 136 } },
  { id: 'reason', cls: 'c-reason', w: { full: 240 } },
];

const colMode = (w: number): ColMode =>
  (w >= 1240 ? 'full' : w >= 1000 ? 'noreason' : w >= 700 ? 'notime' : 'nosize');

/// The narrowest a path column may be before the table stops shrinking and scrolls sideways instead.
/// Under fixed layout the columns with no <col> width absorb every shortfall, so without a floor they
/// go to zero and the paths vanish entirely rather than the table admitting it has run out of room.
const PATH_MIN = 140;

/// Minimum table width for a column set: everything pinned, plus a floor for each path column. A
/// static number cannot do this job — the pinned total is 858 in `full` and 162 in `nosize`, so one
/// value is either far too wide for the narrow set or no constraint at all for the wide one.
function minWidth(cols: ColDef[], mode: ColMode): number {
  let fixed = 0, flex = 0;
  for (const c of cols) {
    const w = c.w[mode];
    if (w == null) flex++; else fixed += w;
  }
  return fixed + flex * PATH_MIN;
}

/// The column set tracks the scroll container, not the window: collapsing the Overview pane widens the
/// table by 208px (236 → 28) without any window resize.
function useWrapWidth(wrap: HTMLElement | null): number {
  const [w, setW] = useState(1600);
  useLayoutEffect(() => {
    if (!wrap) return;
    const ro = new ResizeObserver(() => setW(wrap.clientWidth));
    ro.observe(wrap);
    setW(wrap.clientWidth);
    return () => ro.disconnect();
  }, [wrap]);
  return w;
}

/// A clickable header. Declared at module scope, not inside PlanTable: a component defined in a
/// render body is a *new type* on every render, so React unmounts and rebuilds every one of these
/// spans — and this table re-renders on every scroll frame.
function SortHead({ k, sort, onSort }: { k: SortKey; sort: Sort | null; onSort: (k: SortKey) => void }) {
  const on = sort?.key === k;
  return (
    <span className={'sortable' + (on ? ' on' : '')} onClick={() => onSort(k)}>
      {COL_HEAD[k]}
      {on && (
        <span className="sortmark">
          {sort.dir === 1 ? <ChevronUp size={11} /> : <ChevronDown size={11} />}
        </span>
      )}
    </span>
  );
}

/// indeterminate is a DOM property with no HTML attribute, so it can only be set through a ref
function TriCheckbox(props: { checked: boolean; indeterminate?: boolean; disabled?: boolean; title?: string; onChange: (v: boolean) => void; stopClick?: boolean }) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => { if (ref.current) ref.current.indeterminate = !!props.indeterminate; });
  return (
    <input
      ref={ref}
      type="checkbox"
      checked={props.checked}
      disabled={props.disabled}
      title={props.title}
      onClick={props.stopClick ? (e) => e.stopPropagation() : undefined}
      onChange={(e) => props.onChange(e.target.checked)}
    />
  );
}

/// What a column contributes to one row. The <td> itself is emitted by the single loop below, so
/// the cell count can never disagree with the <col> count.
interface Cell { cls?: string; title?: string; children?: ReactNode }

/// Both meta cells carry the whole truth in their tooltip regardless of mode, so the contract does
/// not change with the window width — that is what lets the time column drop without losing it.
const metaTitle = (sm: SideMeta) => `${sm.size.toLocaleString()} bytes\n${new Date(sm.mtime_ms).toLocaleString()}`;
const ABSENT: Cell = { cls: 'mono dim', children: '—' };

function sizeCell(sm: SideMeta | null, tint: boolean): Cell {
  if (!sm) return ABSENT;
  return { cls: 'mono' + (tint ? ' newer' : ''), title: metaTitle(sm), children: humanSize(sm.size) };
}

function timeCell(sm: SideMeta | null, tint: boolean): Cell {
  if (!sm) return ABSENT;
  return { cls: 'mono' + (tint ? ' newer' : ''), title: metaTitle(sm), children: fmtTime(sm.mtime_ms) };
}

export function PlanTable(props: Props) {
  const {
    plan, flipped, checked, rowPlan, visible, pathMode, grouped, sort, collapsedDirs, resetKey, wrap,
    onToggleRow, onToggleMany, onFlip, onFoldDir, onSort, onContextRow,
  } = props;

  const theadRef = useRef<HTMLTableSectionElement>(null);
  const bodyRef = useRef<HTMLTableSectionElement>(null);

  useEffect(() => { if (wrap) wrap.scrollTop = 0; }, [resetKey, wrap]);

  const win = useVirtualRows(rowPlan, wrap, theadRef, bodyRef);
  const mode = colMode(useWrapWidth(wrap));
  const cols = COLS.filter((c) => c.w[mode] !== undefined);
  const shown = new Set<ColId>(cols.map((c) => c.id));
  const nCols = cols.length;
  const tableMinWidth = minWidth(cols, mode);

  const colGroup = () => (
    <colgroup>
      {cols.map((c) => {
        const w = c.w[mode];
        return <col key={c.id} style={w != null ? { width: w } : undefined} />;
      })}
    </colgroup>
  );

  // Scrolling updates this component's local virtual-window state every animation frame. These two
  // full-plan passes used to run on every one of those renders; at several hundred thousand rows,
  // trackpad momentum allocated another multi-megabyte index array per frame until WebKit hit
  // memory pressure and painted the window black.
  const selectableVisible = useMemo(
    () => visible.filter((i) => selectable(eff(plan, flipped, i))),
    [plan, flipped, visible],
  );
  const allChecked = useMemo(
    () => selectableVisible.length > 0 && selectableVisible.every((i) => checked[i]),
    [selectableVisible, checked],
  );

  /// Where "this side is newer" gets painted: the claim is about the mtime, so it takes the most
  /// specific of that side's columns still on screen, falling back to the path once the narrowest
  /// rung has dropped both meta columns. Without that fallback the single most decision-critical
  /// hint in the table would simply vanish below 700px.
  const newerCol = (side: 's' | 't'): ColId =>
    (shown.has(`${side}.mtime`) ? `${side}.mtime` : shown.has(`${side}.size`) ? `${side}.size` : `${side}.path`);

  const rows = (
    <tbody ref={bodyRef}>
        {rowPlan.slice(win.from, win.to).map((spec, k) => {
          // Zebra striping keys off the real row index, not :nth-child — otherwise the stripes flip as
          // the window scrolls
          const alt = (win.from + k) % 2 === 1;

          if (typeof spec !== 'number') {
            // sel and bytes are computed once when the layout is built, not per render: this row is
            // re-rendered on every scroll frame it is on screen
            const { sel, bytes } = spec;
            const nChecked = sel.filter((i) => checked[i]).length;
            const folded = collapsedDirs.has(spec.dir);
            const label = spec.dir === '' ? '(root)' : spec.dir;
            // Keyed by the dir, which is unique — one group per directory — and stable across a
            // re-sort. An index-based key would change identity whenever the order changed, and
            // remount every group row for nothing.
            return (
              <tr key={`g:${spec.dir}`} className="grp" onClick={() => onFoldDir(spec.dir)}>
                <td className="c-chk">
                  <TriCheckbox
                    checked={sel.length > 0 && nChecked === sel.length}
                    indeterminate={nChecked > 0 && nChecked < sel.length}
                    disabled={sel.length === 0}
                    title="Check / uncheck the whole directory"
                    stopClick
                    onChange={(v) => onToggleMany(sel, v)}
                  />
                </td>
                <td colSpan={nCols - 1} title={`${plan.header.source_root}\n${plan.header.target_root}\n… ${label}`}>
                  <span className="gchev">{folded ? <ChevronRight size={13} /> : <ChevronDown size={13} />}</span>
                  <span className="gdir mono">{label}</span>
                  <span className="gmeta">{spec.items.length} items{bytes ? ` · ${humanSize(bytes)}` : ''}</span>
                </td>
              </tr>
            );
          }

          const i = spec;
          const op = eff(plan, flipped, i);
          const groupDir = grouped ? dirOf(op.path) : null;
          const flippable = canFlip(plan, i);
          const act = rowAction(op);
          const [sp, tp] = sidePaths(op);
          const m = metaOf(plan, i);
          const newer = newerSide(plan, i);
          const tinted = newer ? newerCol(newer) : null;

          // Files inside a group show only their name, while a cross-directory move source keeps its
          // full relative path so nothing is lost. When this side's size column has dropped out, the
          // path tooltip absorbs its numbers — dropped information always lands on the same side.
          const pathCell = (pv: string | null, root: string, sm: SideMeta | null, side: 's' | 't'): Cell => {
            if (!pv) return { cls: 'mono dim' };
            const text = groupDir !== null && dirOf(pv) === groupDir
              ? baseOf(pv)
              : pathMode === 'full' ? fullPath(root, pv) : pv;
            const abs = fullPath(root, pv);
            const metaGone = !shown.has(`${side}.size`);
            return {
              cls: 'mono' + (tinted === `${side}.path` ? ' newer' : ''),
              title: sm && metaGone ? `${abs}\n${metaTitle(sm)}` : abs,
              children: text,
            };
          };

          // Built for every column, rendered for the ones this mode shows — here rather than in a
          // per-column render function because every value it needs is already a local.
          const cells: Record<ColId, Cell> = {
            chk: {
              children: (
                <TriCheckbox checked={checked[i]} disabled={!selectable(op)} onChange={(v) => onToggleRow(i, v)} />
              ),
            },
            's.path': pathCell(sp, plan.header.source_root, m.src, 's'),
            's.size': sizeCell(m.src, tinted === 's.size'),
            's.mtime': timeCell(m.src, tinted === 's.mtime'),
            action: {
              // With the reason column folded away, the action cell's tooltip carries the reason
              title: [
                shown.has('reason') ? '' : op.reason,
                flippable ? 'Click to reverse the direction (click again to restore)' : '',
              ].filter(Boolean).join('\n') || undefined,
              children: (
                <span
                  className={`act k-${act.kind}${flippable ? ' flippable' : ''}`}
                  onClick={flippable ? () => onFlip(i) : undefined}
                >
                  {/* Both glyph slots are always rendered at a fixed width: a conflict has no
                      direction, and without a reserved slot its label would start 16px to the left
                      of every other row's — the arrow is the glyph you scan down this column, so its
                      x has to be the same on every row. */}
                  <span className="act-dir">{act.dir ? DIR_ICON[act.dir] : null}</span>
                  <span className="act-mark">{MARK[act.kind]}</span>
                  <span className="act-label">{act.label}</span>
                </span>
              ),
            },
            't.path': pathCell(tp, plan.header.target_root, m.dst, 't'),
            't.size': sizeCell(m.dst, tinted === 't.size'),
            't.mtime': timeCell(m.dst, tinted === 't.mtime'),
            reason: { children: op.reason },
          };

          return (
            <tr
              key={`r:${i}`}
              className={[!checked[i] && 'off', flipped[i] && 'flip', groupDir !== null && 'ingrp', alt && 'alt']
                .filter(Boolean).join(' ')}
              onContextMenu={(e) => { e.preventDefault(); onContextRow(i, e.clientX, e.clientY); }}
            >
              {cols.map((c) => {
                const cell = cells[c.id];
                return (
                  <td key={c.id} className={cell.cls ? `${c.cls} ${cell.cls}` : c.cls} title={cell.title}>
                    {cell.children}
                  </td>
                );
              })}
            </tr>
          );
        })}
    </tbody>
  );

  // `useVirtualRows` maps the complete logical list onto a bounded physical canvas. Do not put the
  // logical total or row offset directly into CSS: that recreates a multi-million-pixel render space
  // even though only one screenful of rows is mounted.
  return (
    <div
      className="vtable-canvas"
      style={{ minWidth: tableMinWidth, height: win.canvasHeight }}
    >
      <table className="plantable vtable-head">
        {colGroup()}
        <thead ref={theadRef}>
          <tr>
            {cols.map((c) => (
              <th key={c.id} className={c.cls} title={c.headTitle}>
                {c.id === 'chk' ? (
                  <TriCheckbox
                    checked={allChecked}
                    title="Select all / none (current view)"
                    onChange={(v) => onToggleMany(selectableVisible, v)}
                  />
                ) : (
                  <>
                    <SortHead k={c.id} sort={sort} onSort={onSort} />
                    {(c.adopts?.[mode] ?? []).map((k) => (
                      <span key={k}> · <SortHead k={k} sort={sort} onSort={onSort} /></span>
                    ))}
                  </>
                )}
              </th>
            ))}
          </tr>
        </thead>
      </table>
      <table
        className="plantable vtable-body"
        style={{ transform: `translate3d(0, ${win.bodyTop}px, 0)` }}
      >
        {colGroup()}
        {rows}
      </table>
    </div>
  );
}
