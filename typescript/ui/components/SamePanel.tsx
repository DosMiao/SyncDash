import { useCallback, useEffect, useState } from 'react';
import { ArrowLeft } from 'lucide-react';
import { fmtTime, humanSize } from '../../core/format';
import { listSame } from '../../core/ipc';
import { MTIME_SLACK } from '../../core/plan';
import type { SameRow } from '../../core/ipc';

const PAGE = 300;

/// Reads the snapshot left by the last compare, paginated. **No rescan** — both sides were just walked,
/// and walking the trees again just to glance at the identical items would be pure waste.
export function SamePanel({ jobName, onClose }: { jobName: string; onClose: () => void }) {
  const [q, setQ] = useState('');
  const [query, setQuery] = useState('');
  const [rows, setRows] = useState<SameRow[]>([]);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState('');

  useEffect(() => {
    const t = setTimeout(() => setQuery(q.trim()), 250);
    return () => clearTimeout(t);
  }, [q]);

  const load = useCallback(async (offset: number) => {
    try {
      const page = await listSame(jobName, query, offset, PAGE);
      setError('');
      setTotal(page.total);
      setRows((prev) => (offset === 0 ? page.rows : [...prev, ...page.rows]));
    } catch (e) {
      setError(String(e));
      setRows([]);
      setTotal(0);
    }
  }, [jobName, query]);

  useEffect(() => { void load(0); }, [load]);

  return (
    <div className="samepanel">
      <div className="sp-head">
        <span>Files identical on both sides</span>
        <input
          className="sp-q mono"
          type="text"
          placeholder="Filter paths…"
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
        <span className="sp-count dim">{error ? '' : `${rows.length} / ${total.toLocaleString()}`}</span>
        <button className="btn" onClick={onClose}><ArrowLeft size={12} /> Back to differences</button>
      </div>
      <table className="sametable">
        <thead>
          <tr>
            <th className="c-path">Relative path</th>
            <th className="c-size">Size</th>
            <th className="c-meta">source time</th>
            <th className="c-meta">target time</th>
          </tr>
        </thead>
        <tbody>
          {error && <tr><td colSpan={4} className="dim">{error}</td></tr>}
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
        <button className="btn sp-more" onClick={() => void load(rows.length)}>Load more</button>
      )}
    </div>
  );
}
