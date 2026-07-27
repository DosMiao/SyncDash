import { useMemo } from 'react';
import { humanSize } from '../../core/format';
import { eff } from '../../core/plan';
import type { PlanDto } from '../../core/plan';

interface Props {
  plan: PlanDto | null;
  flipped: boolean[];
  collapsed: boolean;
  ovFilter: string | null;
  expanded: Set<string>;
  onToggleCollapsed: () => void;
  onFilter: (key: string | null) => void;
  onToggleExpanded: (key: string) => void;
}

interface Agg { items: number; bytes: number; children: Map<string, { items: number; bytes: number }> }

/// M3: aggregate by top-level directory (count/bytes/share bar); click filters the diff table, the
/// chevron lazily expands a second level.
export function Overview(props: Props) {
  const { plan, flipped, collapsed, ovFilter, expanded, onToggleCollapsed, onFilter, onToggleExpanded } = props;

  const { rows, share } = useMemo(() => {
    const groups = new Map<string, Agg>();
    let totBytes = 0, totItems = 0;
    if (plan) {
      for (let i = 0; i < plan.ops.length; i++) {
        const op = eff(plan, flipped, i);
        const slash = op.path.indexOf('/');
        const seg = slash < 0 ? '(root)' : op.path.slice(0, slash);
        let g = groups.get(seg);
        if (!g) { g = { items: 0, bytes: 0, children: new Map() }; groups.set(seg, g); }
        g.items++;
        g.bytes += op.size ?? 0;
        totItems++;
        totBytes += op.size ?? 0;
        if (slash >= 0) {
          const rest = op.path.slice(slash + 1);
          const slash2 = rest.indexOf('/');
          const seg2 = slash2 < 0 ? '(files)' : rest.slice(0, slash2);
          const c = g.children.get(seg2) ?? { items: 0, bytes: 0 };
          c.items++;
          c.bytes += op.size ?? 0;
          g.children.set(seg2, c);
        }
      }
    }
    const sorted = [...groups.entries()].sort((a, b) => b[1].bytes - a[1].bytes || b[1].items - a[1].items);
    return {
      rows: sorted,
      share: (b: number, n: number) => (totBytes > 0 ? b / totBytes : totItems > 0 ? n / totItems : 0),
    };
  }, [plan, flipped]);

  const Row = ({ rowKey, label, items, bytes, depth, hasKids }: {
    rowKey: string; label: string; items: number; bytes: number; depth: number; hasKids: boolean;
  }) => (
    <div
      className={'ovrow' + (ovFilter === rowKey ? ' on' : '') + (depth ? ' ovchild' : '')}
      onClick={(e) => {
        if (hasKids && (e.target as HTMLElement).classList.contains('chev')) onToggleExpanded(rowKey);
        else onFilter(ovFilter === rowKey ? null : rowKey);
      }}
    >
      <div className="l1">
        <span className="chev">{hasKids ? (expanded.has(rowKey) ? '▾' : '▸') : ''}</span>
        <span className="nm" title={label}>{label}</span>
        <span className="ct">{items} · {humanSize(bytes) || '0 B'}</span>
      </div>
      {/* Width through the style prop, i.e. CSSOM — a style="" attribute is blocked by the nonce CSP */}
      <div className="ovbar"><div style={{ width: `${Math.round(share(bytes, items) * 100)}%` }} /></div>
    </div>
  );

  return (
    <aside id="overview" className={collapsed ? 'collapsed' : ''}>
      <button id="ov-toggle" title="Overview aggregation (expand / collapse)" onClick={onToggleCollapsed}>☰</button>
      <div id="ov-body">
        <div className="ov-head">
          <span>Overview</span>
          <span id="ov-clear" title="Clear filter" onClick={() => onFilter(null)}>✕</span>
        </div>
        <div id="ov-list">
          {rows.map(([seg, g]) => {
            const hasKids = seg !== '(root)' && g.children.size > 0;
            const kids = hasKids && expanded.has(seg)
              ? [...g.children.entries()].sort((a, b) => b[1].bytes - a[1].bytes || b[1].items - a[1].items)
              : [];
            return (
              <div key={seg}>
                <Row rowKey={seg} label={seg} items={g.items} bytes={g.bytes} depth={0} hasKids={hasKids} />
                {kids.filter(([s2]) => s2 !== '(files)').map(([seg2, c]) => (
                  <Row key={seg2} rowKey={`${seg}/${seg2}`} label={seg2} items={c.items} bytes={c.bytes} depth={1} hasKids={false} />
                ))}
              </div>
            );
          })}
        </div>
      </div>
    </aside>
  );
}
