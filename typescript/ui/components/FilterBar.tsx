import { useEffect, useMemo, useRef, useState } from 'react';
import {
  ArrowUpDown,
  ChevronsDownUp,
  ChevronsUpDown,
  Download,
  Equal,
  FolderTree,
  List,
  ListFilter,
  Route,
  X,
} from 'lucide-react';
import { CHIPS, SORT_LABEL, category, eff } from '../../core/plan';
import { MARK } from '../icons';
import type { Chip, PlanDto, Sort } from '../../core/plan';

interface Props {
  plan: PlanDto;
  flipped: boolean[];
  chips: Set<Chip>;
  onChips: (next: Set<Chip>) => void;
  onSearch: (q: string) => void;
  /// Cleared search box when the job changes; keyed so the input resets with it
  searchKey: string;
  funnelCount: number;
  funnelOpen: boolean;
  sameOpen: boolean;
  grouped: boolean;
  sort: Sort | null;
  anyCollapsed: boolean;
  pathMode: 'rel' | 'full';
  onToggleFunnel: (anchor: DOMRect) => void;
  onToggleSame: () => void;
  onExportCsv: () => void;
  onToggleFold: () => void;
  onToggleGroup: () => void;
  onClearSort: () => void;
  onTogglePathMode: () => void;
}

export function FilterBar(props: Props) {
  const {
    plan, flipped, chips, onChips, onSearch, searchKey, funnelCount, funnelOpen, sameOpen,
    grouped, sort, anyCollapsed, pathMode,
    onToggleFunnel, onToggleSame, onExportCsv, onToggleFold, onToggleGroup, onClearSort, onTogglePathMode,
  } = props;

  const [q, setQ] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => { setQ(''); onSearch(''); }, [searchKey, onSearch]);

  // Re-laying out a table of thousands of rows on every keystroke is too heavy
  useEffect(() => {
    const t = setTimeout(() => onSearch(q.trim()), 150);
    return () => clearTimeout(t);
  }, [q, onSearch]);

  // Ctrl+F focuses the box; the shortcut is registered at the app root and reaches us through this ref
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') { e.preventDefault(); inputRef.current?.focus(); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const counts = useMemo(() => {
    const m = new Map<Chip, number>();
    for (let i = 0; i < plan.ops.length; i++) {
      const c = category(eff(plan, flipped, i));
      m.set(c, (m.get(c) ?? 0) + 1);
    }
    m.set('all', plan.ops.length);
    return m;
  }, [plan, flipped]);

  const toggle = (key: Chip) => {
    const next = new Set(chips);
    if (key === 'all') next.clear();
    else if (next.has(key)) next.delete(key);
    else next.add(key);
    onChips(next);
  };

  return (
    <section className="filterbar" aria-label="Filter and display controls">
      <input
        ref={inputRef}
        className="search mono"
        type="search"
        aria-label="Search synchronization plan"
        title="Filter the rows by path, source path or reason (Ctrl+F)"
        placeholder="Search path / from / reason"
        value={q}
        onChange={(e) => setQ(e.target.value)}
      />
      <div className="chips" role="group" aria-label="Action category filters">
        {CHIPS.map(([key, label]) => {
          const n = counts.get(key) ?? 0;
          // The category filter is a set of **independent toggles**, not a radio group; "All" just clears it
          const on = key === 'all' ? chips.size === 0 : chips.has(key);
          return (
            <button
              type="button"
              key={key}
              // 'All' gets no hue class and no glyph: it is the absence of a filter, not a sixth
              // category, and marking it as one would assert it belongs to a vocabulary it doesn't
              className={['chip', key !== 'all' && `k-${key}`, on && 'on', n === 0 && 'zero'].filter(Boolean).join(' ')}
              aria-pressed={on}
              title={key === 'all' ? 'Clear category filter' : 'Toggle for this category (can be on alongside others)'}
              onClick={() => toggle(key)}
            >{key !== 'all' && MARK[key]}{label} {n}</button>
          );
        })}
      </div>
      {/* One block, so a bar wrap never breaks between these three: on a narrow window they open the
          second row together, with .fb-right still pushed to its right end */}
      <div className="fb-actions" role="group" aria-label="Result tools">
        {/* Green rather than the usual accent toggle: these two *narrow what you are looking at*,
            and a filter you forgot you left on is the one piece of state worth colour-coding apart
            from every other latched button in the bar */}
        <button
          type="button"
          className={'btn' + (funnelCount > 0 || funnelOpen ? ' on-green' : '')}
          title="Filter: name masks / size / modified time (applies to the current results, no rescan)"
          aria-expanded={funnelOpen}
          aria-pressed={funnelCount > 0}
          onClick={(e) => { e.stopPropagation(); onToggleFunnel(e.currentTarget.getBoundingClientRect()); }}
        >
          <ListFilter size={12} />
          {funnelCount ? `Filter ${funnelCount}` : 'Filter'}
        </button>
        <button
          type="button"
          className={'btn' + (sameOpen ? ' on-green' : '')}
          title="View the files judged identical on both sides (no rescan, reads the last compare's snapshot)"
          aria-pressed={sameOpen}
          onClick={onToggleSame}
        ><Equal size={12} /> Identical</button>
        <button
          type="button"
          className="btn"
          title="Export the current view as CSV (UTF-8 with BOM, opens straight in Excel)"
          onClick={onExportCsv}
        ><Download size={12} /> CSV</button>
      </div>
      {/* The auto margin lives on this group, not on its first child: which buttons are present here
          varies with the view state, and a margin on an unmounted element pushes nothing. */}
      <div className="fb-right" role="group" aria-label="Plan layout">
        {/* Its own control, not a mode of the group button: a sort now coexists with grouping, so
            the two states need to be readable — and clearable — at the same time. This is also the
            only way to clear a sort whose column the responsive layout has folded away. */}
        {sort && (
          <button
            type="button"
            className="btn on"
            title={`Sorted by ${SORT_LABEL[sort.key]}, ${sort.dir === 1 ? 'ascending' : 'descending'}. Click to clear.`}
            aria-label={`Clear sorting by ${SORT_LABEL[sort.key]}`}
            onClick={onClearSort}
          >
            <ArrowUpDown size={12} />
            Sorted: {SORT_LABEL[sort.key]}
            <X size={12} />
          </button>
        )}
        {grouped && (
          <button type="button" className="btn" title="Collapse / expand every folder in the tree" onClick={onToggleFold}>
            {anyCollapsed ? <ChevronsUpDown size={12} /> : <ChevronsDownUp size={12} />}
            {anyCollapsed ? 'Expand all' : 'Collapse all'}
          </button>
        )}
        <button
          type="button"
          className={'btn' + (grouped ? ' on' : '')}
          title="Toggle: hierarchical directory tree (parents, subfolders, files) ↔ flat list"
          aria-pressed={grouped}
          onClick={onToggleGroup}
        >
          {grouped ? <FolderTree size={12} /> : <List size={12} />}
          {grouped ? 'Folder tree' : 'Flat list'}
        </button>
        <button
          type="button"
          className="btn"
          title="Toggle: relative (to the compare root) ↔ full path"
          aria-pressed={pathMode === 'full'}
          onClick={onTogglePathMode}
        >
          <Route size={12} />
          {pathMode === 'rel' ? 'Relative paths' : 'Full paths'}
        </button>
      </div>
    </section>
  );
}
