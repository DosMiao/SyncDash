import { useId, useRef } from 'react';
import { formatFileTimestamp, humanSize } from '#core/shared/format.ts';
import { MTIME_SLACK_MS } from '#core/domain/compare/plan.ts';
import { useInteractionLayer } from '#ui/shared/interaction/useInteractionLayer.tsx';
import type { IdenticalWorkspace } from '#core/application/compare-workspace/compareWorkspaceModel.ts';

interface IdenticalResultsPanelProps {
  workspace: IdenticalWorkspace;
  onSearchDraftChange: (draft: string) => void;
  onLoadMore: () => void;
  onRetry: () => void;
}

export function IdenticalResultsPanel(props: IdenticalResultsPanelProps) {
  const { workspace, onSearchDraftChange, onLoadMore, onRetry } = props;
  const pages = workspace.pages;
  const rows = pages.status === 'ready'
    || pages.status === 'loading_more'
    || pages.status === 'load_more_failed'
    ? pages.rows
    : [];
  const total = pages.status === 'ready'
    || pages.status === 'loading_more'
    || pages.status === 'load_more_failed'
    ? pages.total
    : 0;
  const loading = pages.status === 'loading_initial' || pages.status === 'loading_more';
  const error = pages.status === 'initial_failed' || pages.status === 'load_more_failed'
    ? pages.error
    : '';
  const searchRef = useRef<HTMLInputElement>(null);
  const titleId = useId();
  useInteractionLayer({
    kind: 'workspace',
    handlers: { find: () => searchRef.current?.focus() },
  });

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
          value={workspace.searchDraft}
          onChange={(event) => onSearchDraftChange(event.target.value)}
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
                {workspace.appliedSearch ? 'No identical files match this filter.' : 'This comparison found no identical files.'}
              </td>
            </tr>
          )}
          {rows.map((row) => {
            const timestampDrift = Math.abs(row.source_mtime_ms - row.target_mtime_ms) > MTIME_SLACK_MS;
            return (
              <tr key={row.path}>
                <td className="mono c-path" title={row.path}>{row.path}</td>
                <td className="c-size mono">{humanSize(row.size)}</td>
                <td className="c-meta mono">{formatFileTimestamp(row.source_mtime_ms)}</td>
                <td
                  className={'c-meta mono' + (timestampDrift ? ' drift' : '')}
                  title={timestampDrift ? 'Target timestamp differs from the source by more than 2 seconds' : undefined}
                >
                  {timestampDrift && <span className="sr-only">Timestamp differs beyond tolerance. </span>}
                  {formatFileTimestamp(row.target_mtime_ms)}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {pages.status === 'initial_failed' && (
        <button type="button" className="btn identical-results-more" onClick={onRetry}>Retry</button>
      )}
      {rows.length < total && (
        <button type="button" className="btn identical-results-more" disabled={loading} onClick={onLoadMore}>
          {loading ? 'Loading…' : 'Load More'}
        </button>
      )}
    </section>
  );
}
