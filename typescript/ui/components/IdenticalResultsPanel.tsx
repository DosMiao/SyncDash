import { useEffect, useId, useRef } from 'react';
import { fmtTime, humanSize } from '../../core/format';
import { MTIME_SLACK_MS } from '../../core/plan';
import type { CompareOwner } from '../../core/types/generated/CompareOwner';
import { useIdenticalResults } from '../hooks/useIdenticalResults';

export function IdenticalResultsPanel({ owner }: { owner: CompareOwner }) {
  const { searchDraft, setSearchDraft, rows, total, error, loading, loadMore } = useIdenticalResults(owner);
  const searchRef = useRef<HTMLInputElement>(null);
  const titleId = useId();
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f') {
        event.preventDefault();
        searchRef.current?.focus();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

  return (
    <section className="identical-results-panel" aria-labelledby={titleId} aria-busy={loading}>
      <div className="identical-results-head">
        <h2 id={titleId}>Files Identical on Both Sides</h2>
        <span className="identical-results-drift-legend">
          <i aria-hidden="true" /> Target Timestamp Differs by More Than 2 Seconds
        </span>
        <input
          ref={searchRef}
          className="identical-results-search mono"
          type="search"
          aria-label="Filter Identical File Paths"
          placeholder="Filter paths…"
          value={searchDraft}
          onChange={(event) => setSearchDraft(event.target.value)}
        />
        <span className="identical-results-count dim" aria-live="polite">
          {error ? '' : `${rows.length} / ${total.toLocaleString()}`}
        </span>
      </div>
      <table className="identical-results-table">
        <caption className="sr-only">Files Identical on the Source and Target</caption>
        <thead>
          <tr>
            <th scope="col" className="c-path">Relative Path</th>
            <th scope="col" className="c-size">Size</th>
            <th scope="col" className="c-meta">Source Time</th>
            <th scope="col" className="c-meta">Target Time</th>
          </tr>
        </thead>
        <tbody>
          {loading && rows.length === 0 && <tr><td colSpan={4} className="dim" role="status">Loading identical files…</td></tr>}
          {error && <tr><td colSpan={4} className="dim" role="alert">{error}</td></tr>}
          {!loading && !error && rows.length === 0 && (
            <tr>
              <td colSpan={4} className="dim">
                {searchDraft.trim() ? 'No identical files match this filter.' : 'This comparison found no identical files.'}
              </td>
            </tr>
          )}
          {rows.map((row) => {
            const timestampDrift = Math.abs(row.source_mtime_ms - row.target_mtime_ms) > MTIME_SLACK_MS;
            return (
              <tr key={row.path}>
                <td className="mono c-path" title={row.path}>{row.path}</td>
                <td className="c-size mono">{humanSize(row.size)}</td>
                <td className="c-meta mono">{fmtTime(row.source_mtime_ms)}</td>
                <td
                  className={'c-meta mono' + (timestampDrift ? ' drift' : '')}
                  title={timestampDrift ? 'Target timestamp differs from the source by more than 2 seconds' : undefined}
                >
                  {timestampDrift && <span className="sr-only">Timestamp differs beyond tolerance. </span>}
                  {fmtTime(row.target_mtime_ms)}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {rows.length < total && (
        <button type="button" className="btn identical-results-more" disabled={loading} onClick={() => void loadMore()}>
          {loading ? 'Loading…' : 'Load More'}
        </button>
      )}
    </section>
  );
}
