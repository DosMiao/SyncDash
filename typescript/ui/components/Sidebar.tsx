import { useRef } from 'react';
import { Circle, Pencil, Plus } from 'lucide-react';
import { relTime } from '../../core/format';
import type { JobDto } from '../../core/types/generated/JobDto';
import type { RunRecord } from '../../core/types/generated/RunRecord';

interface Props {
  jobs: JobDto[];
  currentJobId: string | null;
  /// job name → most recent run, for the second line on each card
  lastMap: Record<string, RunRecord>;
  busy: boolean;
  appVersion: string;
  jobsDir: string;
  onSelect: (job: JobDto) => void;
  onEdit: (name: string) => void;
  onNew: () => void;
}

function LastRun({ r }: { r: RunRecord }) {
  const dot = r.errors > 0 ? 'err' : r.cancelled ? 'warn' : 'ok';
  const stale = Date.now() - r.ts_ms > 7 * 86400_000;
  const note = r.errors > 0 ? ` · ${r.errors} errors` : r.cancelled ? ' · cancelled' : '';
  return (
    <span className={'jrow2' + (stale ? ' stale' : '')}>
      {/* Filled rather than outlined: at 7px an outlined ring reads as a smudge, and this dot is
          carrying the whole outcome of the last run */}
      <span className={'dot ' + dot} aria-hidden="true"><Circle size={7} fill="currentColor" strokeWidth={0} /></span>
      {relTime(r.ts_ms)} · {r.done} items{note}
    </span>
  );
}

export function Sidebar(props: Props) {
  const { jobs, currentJobId, lastMap, busy, appVersion, jobsDir, onSelect, onEdit, onNew } = props;
  const jobButtons = useRef<Array<HTMLButtonElement | null>>([]);

  const moveFocus = (from: number, direction: -1 | 1 | 'first' | 'last') => {
    if (jobs.length === 0) return;
    const next = direction === 'first' ? 0
      : direction === 'last' ? jobs.length - 1
      : (from + direction + jobs.length) % jobs.length;
    jobButtons.current[next]?.focus();
  };

  return (
    <aside className="sidebar" aria-label="SyncDash jobs">
      <div className="brand">Sync<span>Dash</span></div>
      <nav className="joblist" aria-label="Jobs">
        {jobs.map((j, index) => {
          const active = currentJobId === j.job_id;
          return (
          <div
            key={j.job_id}
            className={'job' + (active ? ' active' : '')}
            title={`${j.source}\n→ ${j.target}` + (j.remote ? '\n(applied by a peer syncdash over ssh)' : '')}
          >
            <button
              ref={(element) => { jobButtons.current[index] = element; }}
              type="button"
              className="job-select"
              disabled={busy}
              aria-current={active ? 'page' : undefined}
              tabIndex={active || (!currentJobId && index === 0) ? 0 : -1}
              onClick={() => { if (!active) onSelect(j); }}
              onKeyDown={(e) => {
                if (e.key === 'ArrowDown' || e.key === 'ArrowRight') {
                  e.preventDefault();
                  moveFocus(index, 1);
                } else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') {
                  e.preventDefault();
                  moveFocus(index, -1);
                } else if (e.key === 'Home' || e.key === 'End') {
                  e.preventDefault();
                  moveFocus(index, e.key === 'Home' ? 'first' : 'last');
                }
              }}
            >
              <span className="jrow1">
                <span className="name">{j.name}</span>
                {j.remote && <span className="rbadge">ssh</span>}
                {j.rigor && j.rigor !== 'standard' && <span className="rigor">{j.rigor}</span>}
                <span className={'mode ' + j.mode}>{j.mode}</span>
              </span>
              {lastMap[j.name] && <LastRun r={lastMap[j.name]} />}
            </button>
            <button
              type="button"
              className="jedit"
              aria-label={`Edit job ${j.name}`}
              title="Edit job"
              disabled={busy}
              onClick={() => onEdit(j.name)}
            ><Pencil size={12} aria-hidden="true" /></button>
          </div>
          );
        })}
      </nav>
      <button className="btn newjob" disabled={busy} onClick={onNew}><Plus size={13} /> New job</button>
      <div className="sidefoot">
        <span>{appVersion}</span>
        <span title={jobsDir}>{jobsDir}</span>
      </div>
    </aside>
  );
}
