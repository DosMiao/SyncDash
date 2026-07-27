import { useEffect, useRef } from 'react';
import { baseOf, dirOf, fmtTime, fullPath, humanSize } from '../../core/format';
import { badge, canFlip, eff, metaOf, newerSide, selectable, sidePaths } from '../../core/plan';
import { useVirtualRows } from '../hooks/useVirtualRows';
import type { RowSpec } from '../hooks/useVirtualRows';
import type { PlanDto, SortKey, Sort } from '../../core/plan';
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

function MetaCell({ sm, isNewer }: { sm: SideMeta | null; isNewer: boolean }) {
  if (!sm) return <td className="c-meta mono dim">—</td>;
  return (
    <td
      className={'c-meta mono' + (isNewer ? ' newer' : '')}
      title={`${sm.size.toLocaleString()} bytes\n${new Date(sm.mtime_ms).toLocaleString()}`}
    >{humanSize(sm.size)} · {fmtTime(sm.mtime_ms)}</td>
  );
}

export function PlanTable(props: Props) {
  const {
    plan, flipped, checked, rowPlan, visible, pathMode, sort, collapsedDirs, resetKey,
    onToggleRow, onToggleMany, onFlip, onFoldDir, onSort, onContextRow,
  } = props;

  const wrapRef = useRef<HTMLElement | null>(null);
  const theadRef = useRef<HTMLTableSectionElement>(null);
  const bodyRef = useRef<HTMLTableSectionElement>(null);

  // The scroll container is #tablewrap, which the table does not own; find it once on mount
  useEffect(() => { wrapRef.current = document.getElementById('tablewrap'); }, []);
  useEffect(() => { if (wrapRef.current) wrapRef.current.scrollTop = 0; }, [resetKey]);

  const win = useVirtualRows(rowPlan, wrapRef, theadRef, bodyRef);

  const selectableVisible = visible.filter((i) => selectable(eff(plan, flipped, i)));
  const allChecked = selectableVisible.length > 0 && selectableVisible.every((i) => checked[i]);

  const sortMark = (key: SortKey) => (sort?.key === key ? (sort.dir === 1 ? '▲' : '▼') : '');
  const Sortable = ({ k, children }: { k: SortKey; children: React.ReactNode }) => (
    <span
      className={'sortable' + (sort?.key === k ? ' on' : '')}
      data-dir={sortMark(k)}
      onClick={() => onSort(k)}
    >{children}</span>
  );

  return (
    <table id="plantable">
      {/* Column widths are pinned here: the body is virtually scrolled, and auto layout recomputes
          widths from whichever rows happen to be mounted, so the columns jump on every scroll.
          The two w-path columns get no width and split the remaining space. */}
      <colgroup>
        <col className="w-chk" /><col className="w-act" /><col className="w-path" /><col className="w-meta" />
        <col className="w-path" /><col className="w-meta" /><col className="w-reason" />
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
            <Sortable k="action">action</Sortable> <span className="dim thin">(click a badge to flip direction)</span>
          </th>
          <th className="c-path"><Sortable k="path">source side</Sortable></th>
          <th className="c-meta"><Sortable k="s.size">Size</Sortable> · <Sortable k="s.mtime">Time</Sortable></th>
          <th className="c-path"><Sortable k="path">target side</Sortable></th>
          <th className="c-meta"><Sortable k="t.size">Size</Sortable> · <Sortable k="t.mtime">Time</Sortable></th>
          <th>reason</th>
        </tr>
      </thead>
      <tbody ref={bodyRef}>
        {win.padTop > 0 && <tr className="vspacer"><td colSpan={7} style={{ height: win.padTop }} /></tr>}
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
                <td colSpan={6} title={`${plan.header.source_root}\n${plan.header.target_root}\n… ${label}`}>
                  <span className="gchev">{folded ? '▸' : '▾'}</span>{' '}
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
                  title={flippable ? 'Click to reverse the direction (click again to restore)' : undefined}
                  onClick={flippable ? () => onFlip(i) : undefined}
                >{b.text}</span>
              </td>
              {pathCell(sp, plan.header.source_root)}
              <MetaCell sm={m.src} isNewer={newer === 's'} />
              {pathCell(tp, plan.header.target_root)}
              <MetaCell sm={m.dst} isNewer={newer === 't'} />
              <td className="reason">{op.reason}</td>
            </tr>
          );
        })}
        {win.padBottom > 0 && <tr className="vspacer"><td colSpan={7} style={{ height: win.padBottom }} /></tr>}
      </tbody>
    </table>
  );
}
