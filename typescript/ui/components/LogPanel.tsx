import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react';
import { FolderOpen, Settings2, X } from 'lucide-react';
import { formatLogTimestamp } from '../../core/format';
import { logArtifact, logRuns, revealLogLocation } from '../../core/ipc';
import {
  LOG_ARTIFACT_VIEW_OPTIONS,
  LOG_LEVEL_FILTER_OPTIONS,
  LOG_LEVEL_LABELS,
  formatRunLabel,
  parseLogArtifactLine,
  runHasArtifactSet,
  runHasEventArtifact,
} from '../../core/logs';
import type { LogArtifactView, LogLevelFilter, LogRow } from '../../core/logs';
import type { RunRecord } from '../../core/types/generated/RunRecord';
import type { StatusAppearance } from '../hooks/useStatus';
import { useInteractionLayer } from '../hooks/useInteractionLayer';
import { RequestFence } from '../state/request-fence';

const MAX_RENDERED_LOG_ROWS = 3000;
type RunListLoadState = 'loading' | 'ready' | 'failed';

interface LogPanelProps {
  jobId: string | null;
  onClose: () => void;
  onSettings: () => void;
  onStatus: (message: string, appearance?: StatusAppearance) => void;
  reloadKey: number;
}

export function LogPanel({ jobId, onClose, onSettings, onStatus, reloadKey }: LogPanelProps) {
  const [runRecords, setRunRecords] = useState<RunRecord[]>([]);
  const [selectedRecordId, setSelectedRecordId] = useState<string | null>(null);
  const [followLatest, setFollowLatest] = useState(true);
  const [artifactView, setArtifactView] = useState<LogArtifactView>('run');
  const [levelFilter, setLevelFilter] = useState<LogLevelFilter>('all');
  const [searchQuery, setSearchQuery] = useState('');
  const [logRows, setLogRows] = useState<LogRow[]>([]);
  const [notice, setNotice] = useState('Loading…');
  const [selectionNotice, setSelectionNotice] = useState('');
  const [runListLoadState, setRunListLoadState] = useState<RunListLoadState>('loading');
  const [revealPending, setRevealPending] = useState(false);
  const runsFence = useRef(new RequestFence());
  const artifactFence = useRef(new RequestFence());
  const revealRequest = useRef<symbol | null>(null);
  const loadedJobId = useRef<string | null>(null);
  const followLatestRef = useRef(followLatest);
  followLatestRef.current = followLatest;
  const searchInput = useRef<HTMLInputElement>(null);
  const titleId = useId();
  useInteractionLayer({
    kind: 'auxiliary_panel',
    handlers: {
      dismiss: onClose,
      find: () => searchInput.current?.focus(),
    },
  });

  useEffect(() => () => { revealRequest.current = null; }, []);

  useEffect(() => {
    const ticket = runsFence.current.start(`${jobId ?? '*'}\0${reloadKey}`);
    const jobChanged = loadedJobId.current !== jobId;
    artifactFence.current.invalidate();
    setLogRows([]);
    setNotice('Loading…');
    setRunListLoadState('loading');
    logRuns(jobId)
      .then((loadedRuns) => {
        if (!runsFence.current.owns(ticket)) return;
        setRunRecords(loadedRuns);
        setSelectedRecordId((currentRecordId) => {
          const preserveSelection = !jobChanged && !followLatestRef.current;
          if (preserveSelection && currentRecordId !== null) {
            const retained = loadedRuns.some((record) => record.record_id === currentRecordId);
            if (retained) {
              setSelectionNotice('');
              return currentRecordId;
            }
            if (loadedRuns.length > 0) {
              setSelectionNotice('The selected run is no longer retained; showing the newest remaining run');
            }
          }
          if (jobChanged || loadedRuns.length === 0) setSelectionNotice('');
          return loadedRuns[0]?.record_id ?? null;
        });
        loadedJobId.current = jobId;
        if (jobChanged) {
          setFollowLatest(true);
          followLatestRef.current = true;
        }
        setRunListLoadState('ready');
        if (loadedRuns.length === 0) {
          setNotice('No runs recorded yet — a trace appears only after a job runs (Synchronize)');
        }
      })
      .catch((error) => {
        if (!runsFence.current.owns(ticket)) return;
        setRunRecords([]);
        setRunListLoadState('failed');
        setNotice(`Failed to read the run list: ${error}`);
      });
    return () => { runsFence.current.invalidate(); artifactFence.current.invalidate(); };
  }, [jobId, reloadKey]);

  const selectedRun = runRecords.find((record) => record.record_id === selectedRecordId);

  const loadArtifact = useCallback(async () => {
    setLogRows([]);
    if (runListLoadState !== 'ready') {
      artifactFence.current.invalidate();
      return;
    }
    if (!runRecords.length) {
      artifactFence.current.invalidate();
      setNotice('No runs recorded yet — a trace appears only after a job runs (Synchronize)');
      return;
    }
    if (!selectedRun) { artifactFence.current.invalidate(); return; }
    if (!runHasEventArtifact(selectedRun)) {
      artifactFence.current.invalidate();
      setNotice(selectedRun.artifacts.kind === 'summary_only'
        ? 'Compare runs retain summary evidence only; they do not create detail artifacts'
        : 'Detail persistence was unavailable for this run');
      return;
    }
    const ticket = artifactFence.current.start(`${selectedRun.record_id}\0${artifactView}`);
    setNotice('Loading…');
    try {
      const lines = await logArtifact(selectedRun.record_id, artifactView);
      if (!artifactFence.current.owns(ticket)) return;
      setLogRows(lines
        .map((line) => parseLogArtifactLine(line, artifactView)));
      setNotice('');
    } catch (error) {
      if (!artifactFence.current.owns(ticket)) return;
      setNotice(`Read failed: ${error}`);
    }
  }, [artifactView, runListLoadState, runRecords.length, selectedRun]);

  useEffect(() => { void loadArtifact(); }, [loadArtifact]);

  const revealSelectedLogLocation = useCallback(async () => {
    if (revealRequest.current !== null) return;
    const requestId = Symbol('reveal-log-location');
    const selectedRunId = selectedRun?.record_id ?? null;
    revealRequest.current = requestId;
    setRevealPending(true);
    try {
      await revealLogLocation(selectedRunId);
    } catch (error) {
      if (revealRequest.current === requestId) {
        onStatus(`Failed to open the directory: ${error}`, 'err');
      }
    } finally {
      if (revealRequest.current === requestId) {
        revealRequest.current = null;
        setRevealPending(false);
      }
    }
  }, [onStatus, selectedRun?.record_id]);

  const visibleRows = useMemo(() => {
    const searchNeedle = searchQuery.trim().toLowerCase();
    return logRows.filter((row) =>
      (levelFilter === 'all' || row.level === levelFilter)
      && (!searchNeedle || row.searchText.toLowerCase().includes(searchNeedle)));
  }, [levelFilter, logRows, searchQuery]);

  return (
    <section className="logpanel" aria-labelledby={titleId}>
      <div className="lp-head">
        <h2 className="lp-title" id={titleId}>Log</h2>
        <select
          aria-label="Select a run"
          title="Select a run"
          value={selectedRecordId ?? ''}
          onChange={(event) => {
            setFollowLatest(false);
            setSelectionNotice('');
            setSelectedRecordId(event.target.value || null);
          }}
        >
          {runRecords.map((record) => (
            <option key={record.record_id} value={record.record_id}>{formatRunLabel(record)}</option>
          ))}
        </select>
        <button
          type="button"
          className={'lp-pill' + (followLatest ? ' on' : '')}
          aria-pressed={followLatest}
          onClick={() => {
            setFollowLatest(true);
            setSelectionNotice('');
            setSelectedRecordId(runRecords[0]?.record_id ?? null);
          }}
        >Follow latest</button>
        <div className="lp-pills" role="group" aria-label="Log artifact">
          {LOG_ARTIFACT_VIEW_OPTIONS.map(([option, label]) => (
            <button
              type="button"
              key={option}
              className={'lp-pill' + (option === artifactView ? ' on' : '')}
              aria-pressed={option === artifactView}
              onClick={() => setArtifactView(option)}
            >{label}</button>
          ))}
        </div>
        <div className="lp-pills" role="group" aria-label="Log level">
          {LOG_LEVEL_FILTER_OPTIONS.map(([option, label]) => {
            const count = option === 'all'
              ? logRows.length
              : logRows.filter((row) => row.level === option).length;
            return (
              <button
                type="button"
                key={option}
                className={'lp-pill' + (option === levelFilter ? ' on' : '') + (option !== 'all' ? ` lv-${option}` : '')}
                aria-pressed={option === levelFilter}
                onClick={() => setLevelFilter(option)}
              >{label} {count}</button>
            );
          })}
        </div>
        <input
          ref={searchInput}
          className="mono"
          type="search"
          aria-label="Search log messages and paths"
          placeholder="Search messages / paths…"
          value={searchQuery}
          onChange={(event) => setSearchQuery(event.target.value)}
        />
        <span className="dim" aria-live="polite">
          {logRows.length ? `${visibleRows.length} / ${logRows.length}` : ''}
        </span>
        <span className="spacer" />
        <button
          type="button"
          className="icon-btn"
          title={selectedRun && runHasArtifactSet(selectedRun)
            ? "Open this run's folder in the file manager"
            : 'Open the log directory in the file manager'}
          aria-label={revealPending
            ? 'Opening the log location'
            : selectedRun && runHasArtifactSet(selectedRun)
              ? "Open this run's folder in the file manager"
              : 'Open the log directory in the file manager'}
          disabled={revealPending}
          onClick={() => { void revealSelectedLogLocation(); }}
        ><FolderOpen size={14} /></button>
        <button type="button" className="icon-btn" title="Log settings" aria-label="Log settings" onClick={onSettings}><Settings2 size={14} /></button>
        <button type="button" className="icon-btn" title="Collapse the log panel" aria-label="Collapse the log panel" onClick={onClose}><X size={14} /></button>
      </div>
      {selectionNotice && <div className="log-selection-notice" role="status" aria-live="polite">{selectionNotice}</div>}
      <div className="lp-body" role="region" aria-label="Run log entries" aria-busy={notice === 'Loading…'}>
        {notice && <div className="logempty" role="status" aria-live="polite">{notice}</div>}
        {!notice && visibleRows.length === 0 && (
          <div className="logempty" role="status">{logRows.length ? 'No matching rows' : 'This artifact is empty'}</div>
        )}
        {visibleRows.slice(0, MAX_RENDERED_LOG_ROWS).map((row, index) => (
          <div key={`${row.timestampMs}-${row.scope}-${index}`} className={`lp-row lv-${row.level}`}>
            <span className="lp-t">{row.timestampMs ? formatLogTimestamp(row.timestampMs) : ''}</span>
            <span className="lp-l">{LOG_LEVEL_LABELS[row.level]}</span>
            <span className="lp-s">{row.scope}</span>
            <span className="lp-m">{row.message}</span>
          </div>
        ))}
        {visibleRows.length > MAX_RENDERED_LOG_ROWS && (
          <div className="logempty">
            Showing the first {MAX_RENDERED_LOG_ROWS} of {visibleRows.length} rows — narrow it with search, or open the run
            folder above to read the raw file
          </div>
        )}
      </div>
    </section>
  );
}
