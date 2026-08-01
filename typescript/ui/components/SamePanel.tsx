import { useCallback, useEffect, useId, useRef, useState } from 'react';
import { ArrowLeft } from 'lucide-react';
import { fmtTime, humanSize } from '../../core/format';
import { listSame } from '../../core/ipc';
import { MTIME_SLACK } from '../../core/plan';
import type { SameRow } from '../../core/ipc';
import type { CompareOwner } from '../../core/types/generated/CompareOwner';
import { RequestFence } from '../state/request-fence';

const PAGE = 300;

/// Reads the snapshot left by the last compare, paginated. **No rescan** — both sides were just walked,
/// and walking the trees again just to glance at the identical items would be pure waste.
export function SamePanel({ owner, onClose }: { owner: CompareOwner; onClose: () => void }) {
  const [q, setQ] = useState('');
  const [query, setQuery] = useState('');
  const [rows, setRows] = useState<SameRow[]>([]);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const fence = useRef(new RequestFence());
  const titleId = useId();

  useEffect(() => {
    const t = setTimeout(() => setQuery(q.trim()), 250);
    return () => clearTimeout(t);
  }, [q]);

  const load = useCallback(async (offset: number) => {
    const key = `${owner.compare_id}\0${owner.job_name}\0${owner.target_index}\0${owner.config_revision}\0${query}\0${offset}`;
    const ticket = fence.current.start(key);
    setLoading(true);
    if (offset === 0) {
      setRows([]);
      setTotal(0);
      setError('');
    }
    try {
      const page = await listSame(owner, query, offset, PAGE);
      if (!fence.current.owns(ticket)) return;
      setLoading(false);
      setError('');
      setTotal(page.total);
      setRows((prev) => (offset === 0 ? page.rows : [...prev, ...page.rows]));
    } catch (e) {
      if (!fence.current.owns(ticket)) return;
      setLoading(false);
      setError(String(e));
      setRows([]);
      setTotal(0);
    }
  }, [owner, query]);

  useEffect(() => { void load(0); }, [load]);
  useEffect(() => () => fence.current.invalidate(), []);

  return (
    <section className="samepanel" aria-labelledby={titleId} aria-busy={loading}>
      <div className="sp-head">
        <h2 id={titleId}>Files identical on both sides</h2>
        <input
          className="sp-q mono"
          type="search"
          aria-label="Filter identical file paths"
          placeholder="Filter paths…"
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
        <span className="sp-count dim" aria-live="polite">{error ? '' : `${rows.length} / ${total.toLocaleString()}`}</span>
        <button type="button" className="btn" onClick={onClose}><ArrowLeft size={12} /> Back to differences</button>
      </div>
      <table className="sametable">
        <caption className="sr-only">Files identical on the source and target</caption>
        <thead>
          <tr>
            <th scope="col" className="c-path">Relative path</th>
            <th scope="col" className="c-size">Size</th>
            <th scope="col" className="c-meta">Source time</th>
            <th scope="col" className="c-meta">Target time</th>
          </tr>
        </thead>
        <tbody>
          {loading && rows.length === 0 && <tr><td colSpan={4} className="dim" role="status">Loading identical files…</td></tr>}
          {error && <tr><td colSpan={4} className="dim" role="alert">{error}</td></tr>}
          {rows.map((r, k) => (
            <tr key={`${r.path}:${k}`}>
              <td className="mono c-path" title={r.path}>{r.path}</td>
              <td className="c-size mono">{humanSize(r.size)}</td>
              <td className="c-meta mono">{fmtTime(r.mtime_ms)}</td>
              <td className={'c-meta mono' + (Math.abs(r.mtime_ms - r.other_mtime_ms) > MTIME_SLACK ? ' drift' : '')}>
                {fmtTime(r.other_mtime_ms)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {rows.length < total && (
        <button type="button" className="btn sp-more" disabled={loading} onClick={() => void load(rows.length)}>
          {loading ? 'Loading…' : 'Load more'}
        </button>
      )}
    </section>
  );
}
