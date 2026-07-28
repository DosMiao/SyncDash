import { useCallback, useEffect, useMemo, useState } from 'react';
import { FolderOpen, Settings2, X } from 'lucide-react';
import { hms } from '../../core/format';
import { logArtifact, logDirPath, logRuns, reveal } from '../../core/ipc';
import { LOG_LEVEL_LABEL, LP_LEVELS, LP_VIEWS, parseLogLine, runLabel } from '../../core/logs';
import type { LogLevel, LogRow, LogView } from '../../core/logs';
import type { RunRecord } from '../../core/types/generated/RunRecord';

/// Building DOM for a huge list stalls; the items list routinely runs to tens of thousands of rows
const CAP = 3000;

interface Props {
  jobName: string | null;
  onClose: () => void;
  onSettings: () => void;
  onStatus: (msg: string, cls?: '' | 'err' | 'ok') => void;
  /// Bumped after a run finishes so the panel picks up the new run without being reopened
  reloadKey: number;
}

export function LogPanel({ jobName, onClose, onSettings, onStatus, reloadKey }: Props) {
  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [sel, setSel] = useState(0);
  const [view, setView] = useState<LogView>('run');
  const [level, setLevel] = useState<LogLevel | 'all'>('all');
  const [q, setQ] = useState('');
  const [rows, setRows] = useState<LogRow[]>([]);
  const [notice, setNotice] = useState('Loading…');

  useEffect(() => {
    let live = true;
    logRuns(jobName, 100)
      .then((r) => { if (live) { setRuns(r); setSel(0); } })
      .catch((e) => { if (live) { setRuns([]); setNotice(`Failed to read the run list: ${e}`); } });
    return () => { live = false; };
  }, [jobName, reloadKey]);

  const current = runs[sel];

  const load = useCallback(async () => {
    setRows([]);
    if (!runs.length) {
      setNotice('No runs recorded yet — a trace appears only after a job runs (Synchronize)');
      return;
    }
    if (!current) return;
    if (!current.run_id) {
      // Compare runs leave only an index line and no directory — spell out why this is empty
      setNotice('Compare runs record a summary only, with no detail directory (AutoScan runs every 30 s, and creating a directory each time would flood the log disk)');
      return;
    }
    setNotice('Loading…');
    try {
      const lines = await logArtifact(current.run_id, view);
      setRows(lines.map((l) => parseLogLine(l, view)).filter((x): x is LogRow => x !== null));
      setNotice('');
    } catch (e) {
      setNotice(`Read failed: ${e}`);
    }
  }, [current, runs.length, view]);

  useEffect(() => { void load(); }, [load]);

  const shown = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return rows.filter((r) =>
      (level === 'all' || r.level === level)
      && (!needle || r.hay.toLowerCase().includes(needle) || r.text.toLowerCase().includes(needle)));
  }, [rows, level, q]);

  return (
    <div className="logpanel">
      <div className="lp-head">
        <span className="lp-title">Log</span>
        <select title="Select a run" value={sel} onChange={(e) => setSel(Number(e.target.value))}>
          {runs.map((r, i) => <option key={i} value={i}>{runLabel(r)}</option>)}
        </select>
        <div className="lp-pills">
          {LP_VIEWS.map(([v, label]) => (
            <button key={v} className={'lp-pill' + (v === view ? ' on' : '')} onClick={() => setView(v)}>{label}</button>
          ))}
        </div>
        <div className="lp-pills">
          {LP_LEVELS.map(([l, label]) => {
            const n = l === 'all' ? rows.length : rows.filter((r) => r.level === l).length;
            return (
              <button
                key={l}
                className={'lp-pill' + (l === level ? ' on' : '') + (l !== 'all' ? ` lv-${l}` : '')}
                onClick={() => setLevel(l)}
              >{label} {n}</button>
            );
          })}
        </div>
        <input
          className="mono"
          type="text"
          placeholder="Search messages / paths…"
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
        <span className="dim">{rows.length ? `${shown.length} / ${rows.length}` : ''}</span>
        <span className="spacer" />
        <button
          className="icon-btn"
          title="Open this run's folder in the file manager"
          onClick={() => {
            logDirPath(current?.run_id ?? null)
              .then(reveal)
              .catch((e) => onStatus(`Failed to open the directory: ${e}`, 'err'));
          }}
        ><FolderOpen size={14} /></button>
        <button className="icon-btn" title="Log settings" onClick={onSettings}><Settings2 size={14} /></button>
        <button className="icon-btn" title="Collapse the log panel" onClick={onClose}><X size={14} /></button>
      </div>
      <div className="lp-body">
        {notice && <div className="logempty">{notice}</div>}
        {!notice && shown.length === 0 && (
          <div className="logempty">{rows.length ? 'No matching rows' : 'This artifact is empty'}</div>
        )}
        {shown.slice(0, CAP).map((r, k) => (
          <div key={k} className={`lp-row lv-${r.level}`}>
            <span className="lp-t">{r.ts ? hms(r.ts) : ''}</span>
            <span className="lp-l">{LOG_LEVEL_LABEL[r.level]}</span>
            <span className="lp-s">{r.scope}</span>
            <span className="lp-m">{r.text}</span>
          </div>
        ))}
        {shown.length > CAP && (
          <div className="logempty">
            Showing the first {CAP} of {shown.length} rows — narrow it with search, or open the run
            folder above to read the raw file
          </div>
        )}
      </div>
    </div>
  );
}
