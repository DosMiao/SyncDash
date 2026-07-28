import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ArrowLeft, ArrowRight, ChevronDown, ChevronRight, ChevronUp, Info, Zap } from 'lucide-react';
import { baseOf, dirOf, fmtTime, fullPath, humanSize } from '../../core/format';
import { badge, canFlip, eff, metaOf, newerSide, selectable, sidePaths } from '../../core/plan';
import { useVirtualRows } from '../hooks/useVirtualRows';
import type { ReactNode } from 'react';
import type { RowSpec } from '../hooks/useVirtualRows';
import type { BadgeIcon, PlanDto, SortKey, Sort } from '../../core/plan';
import type { SideMeta } from '../../core/types/generated/SideMeta';

interface Props {
  plan: PlanDto;
  flipped: boolean[];
  checked: boolean[];
  rowPlan: RowSpec[];
  visible: number[];
  pathMode: 'rel' | 'full';
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

/// Responsive column policy. Width goes to information priority: the two path columns are the primary
/// content, so everything else diets first as the container narrows — the reason column folds into the
/// badge tooltip, the action column drops its header hint, then Size · Time drops to size-only (the
/// full pair stays in the cell tooltip). Sized so the paths never fall under ~140px before the table's
/// min-width kicks in and it scrolls sideways instead.
///   full:     | 38 chk | 200 action | path | 178 size·time | path | 178 size·time | 240 reason |
///   noreason: | 38 chk | 140 action | path | 178 size·time | path | 178 size·time |
///   compact:  | 38 chk | 140 action | path |  96 size      | path |  96 size      |
/// 140 = badge min-width 88 + badge padding/border 20 + cell padding 20, with room to spare before
/// badges smear into the next column (fixed layout does not clip). 178 fits "894.0 MB · 07-26 16:51".
type ColMode = 'full' | 'noreason' | 'compact';
const COLW: Record<ColMode, (number | null)[]> = {
  full: [38, 200, null, 178, null, 178, 240],
  noreason: [38, 140, null, 178, null, 178],
  compact: [38, 140, null, 96, null, 96],
};
const colMode = (w: number): ColMode => (w >= 1160 ? 'full' : w >= 900 ? 'noreason' : 'compact');

/// The column set tracks the scroll container, not the window: collapsing the Overview pane widens the
/// table by 224px without any window resize.
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

/// The badge's leading mark. Kept beside the table rather than in core/plan.ts: which glyph draws a
/// direction is a rendering decision, while *which direction it is* is plan semantics.
const BADGE_ICON: Record<BadgeIcon, ReactNode> = {
  right: <ArrowRight size={12} />,
  left: <ArrowLeft size={12} />,
  conflict: <Zap size={12} />,
  note: <Info size={12} />,
};

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

function MetaCell({ sm, isNewer, compact }: { sm: SideMeta | null; isNewer: boolean; compact: boolean }) {
  if (!sm) return <td className="c-meta mono dim">—</td>;
  return (
    <td
      className={'c-meta mono' + (isNewer ? ' newer' : '')}
      title={`${sm.size.toLocaleString()} bytes\n${new Date(sm.mtime_ms).toLocaleString()}`}
    >{compact ? humanSize(sm.size) : `${humanSize(sm.size)} · ${fmtTime(sm.mtime_ms)}`}</td>
  );
}

export function PlanTable(props: Props) {
  const {
    plan, flipped, checked, rowPlan, visible, pathMode, sort, collapsedDirs, resetKey, wrap,
    onToggleRow, onToggleMany, onFlip, onFoldDir, onSort, onContextRow,
  } = props;

  const theadRef = useRef<HTMLTableSectionElement>(null);
  const bodyRef = useRef<HTMLTableSectionElement>(null);

  useEffect(() => { if (wrap) wrap.scrollTop = 0; }, [resetKey, wrap]);

  const win = useVirtualRows(rowPlan, wrap, theadRef, bodyRef);
  const mode = colMode(useWrapWidth(wrap));
  const compact = mode === 'compact';
  const nCols = COLW[mode].length;

  const selectableVisible = visible.filter((i) => selectable(eff(plan, flipped, i)));
  const allChecked = selectableVisible.length > 0 && selectableVisible.every((i) => checked[i]);

  const Sortable = ({ k, children }: { k: SortKey; children: ReactNode }) => (
    <span className={'sortable' + (sort?.key === k ? ' on' : '')} onClick={() => onSort(k)}>
      {children}
      {sort?.key === k && (
        <span className="sortmark">
          {sort.dir === 1 ? <ChevronUp size={11} /> : <ChevronDown size={11} />}
        </span>
      )}
    </span>
  );

  return (
    <table className="plantable">
      {/* Column widths are pinned (via COLW): the body is virtually scrolled, and auto layout would
          recompute widths from whichever rows happen to be mounted, so the columns would jump on every
          scroll. The two path columns get no width and split the remaining space. */}
      <colgroup>
        {COLW[mode].map((w, k) => <col key={k} style={w !== null ? { width: w } : undefined} />)}
      </colgroup>
      <thead ref={theadRef}>
        <tr>
          <th className="c-chk">
            <TriCheckbox
              checked={allChecked}
              title="Select all / none (current view)"
              onChange={(v) => onToggleMany(selectableVisible, v)}
            />
          </th>
          <th className="c-act">
            <Sortable k="action">action</Sortable>
            {mode === 'full' && <> <span className="hint">click a badge to flip</span></>}
          </th>
          <th className="c-path"><Sortable k="path">source side</Sortable></th>
          <th className="c-meta">
            <Sortable k="s.size">Size</Sortable>{!compact && <> · <Sortable k="s.mtime">Time</Sortable></>}
          </th>
          <th className="c-path"><Sortable k="path">target side</Sortable></th>
          <th className="c-meta">
            <Sortable k="t.size">Size</Sortable>{!compact && <> · <Sortable k="t.mtime">Time</Sortable></>}
          </th>
          {mode === 'full' && <th>reason</th>}
        </tr>
      </thead>
      <tbody ref={bodyRef}>
        {win.padTop > 0 && <tr className="vspacer"><td colSpan={nCols} style={{ height: win.padTop }} /></tr>}
        {rowPlan.slice(win.from, win.to).map((spec, k) => {
          // Zebra striping keys off the real row index, not :nth-child — otherwise the stripes flip as
          // the window scrolls
          const alt = (win.from + k) % 2 === 1;

          if (spec.kind === 'grp') {
            const sel = spec.items.filter((i) => selectable(eff(plan, flipped, i)));
            const nChecked = sel.filter((i) => checked[i]).length;
            const folded = collapsedDirs.has(spec.dir);
            let bytes = 0;
            for (const i of spec.items) bytes += eff(plan, flipped, i).size ?? 0;
            const label = spec.dir === '' ? '(root)' : spec.dir;
            // Keyed by the group's first plan index, not the dir: grouping is by contiguous runs, so
            // the same dir can appear as several group rows, and duplicate keys make React orphan
            // fibers as the virtual window slides — their <tr>s pile up in the DOM as frozen ghosts
            return (
              <tr key={`g:${spec.items[0]}`} className="grp" onClick={() => onFoldDir(spec.dir)}>
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

          const i = spec.i;
          const op = eff(plan, flipped, i);
          const flippable = canFlip(plan, i);
          const b = badge(op);
          const [sp, tp] = sidePaths(op);
          const m = metaOf(plan, i);
          const newer = newerSide(plan, i);

          // Files inside a group show only their name, while a cross-directory move source keeps its
          // full relative path so nothing is lost
          const pathCell = (pv: string | null, root: string) => {
            if (!pv) return <td className="mono c-path dim" />;
            const text = spec.groupDir !== null && dirOf(pv) === spec.groupDir
              ? baseOf(pv)
              : pathMode === 'full' ? fullPath(root, pv) : pv;
            return <td className="mono c-path" title={fullPath(root, pv)}>{text}</td>;
          };

          return (
            <tr
              key={`r:${i}`}
              className={[!checked[i] && 'off', flipped[i] && 'flip', spec.groupDir !== null && 'ingrp', alt && 'alt']
                .filter(Boolean).join(' ')}
              onContextMenu={(e) => { e.preventDefault(); onContextRow(i, e.clientX, e.clientY); }}
            >
              <td className="c-chk">
                <TriCheckbox
                  checked={checked[i]}
                  disabled={!selectable(op)}
                  onChange={(v) => onToggleRow(i, v)}
                />
              </td>
              <td className="c-act">
                <span
                  className={'badge ' + b.cls + (flippable ? ' flippable' : '')}
                  // With the reason column folded away, the badge tooltip carries the reason instead
                  title={[
                    mode !== 'full' ? op.reason : '',
                    flippable ? 'Click to reverse the direction (click again to restore)' : '',
                  ].filter(Boolean).join('\n') || undefined}
                  onClick={flippable ? () => onFlip(i) : undefined}
                >{BADGE_ICON[b.icon]}{b.label}</span>
              </td>
              {pathCell(sp, plan.header.source_root)}
              <MetaCell sm={m.src} isNewer={newer === 's'} compact={compact} />
              {pathCell(tp, plan.header.target_root)}
              <MetaCell sm={m.dst} isNewer={newer === 't'} compact={compact} />
              {mode === 'full' && <td className="reason">{op.reason}</td>}
            </tr>
          );
        })}
        {win.padBottom > 0 && <tr className="vspacer"><td colSpan={nCols} style={{ height: win.padBottom }} /></tr>}
      </tbody>
    </table>
  );
}
