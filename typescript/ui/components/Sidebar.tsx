import { useRef } from 'react';
import { Circle, Pencil, Plus } from 'lucide-react';
import { relTime } from '../../core/format';
import type { JobDto } from '../../core/types/generated/JobDto';
import type { RunRecord } from '../../core/types/generated/RunRecord';

interface SidebarProps {
  jobs: JobDto[];
  currentJobId: string | null;
  /// job name → most recent run, for the second line on each card
  lastSyncByJobName: Record<string, RunRecord>;
  busy: boolean;
  /// A safety review may be abandoned by selecting another job, but editing job identity/config
  /// underneath it is disabled until the response is fenced or dismissed.
  reviewing: boolean;
  appVersion: string;
  jobsDir: string;
  onSelect: (job: JobDto) => void;
  onEdit: (name: string) => void;
  onNew: () => void;
}

function LastRunSummary({ record }: { record: RunRecord }) {
  const outcomeClass = record.errors > 0 ? 'err' : record.cancelled ? 'warn' : 'ok';
  const stale = Date.now() - record.ts_ms > 7 * 86400_000;
  const outcomeSuffix = record.errors > 0
    ? ` · ${record.errors} errors`
    : record.cancelled
      ? ' · cancelled'
      : '';
  return (
    <span className={'jrow2' + (stale ? ' stale' : '')}>
      {/* Filled rather than outlined: at 7px an outlined ring reads as a smudge, and this dot is
          carrying the whole outcome of the last run */}
      <span className={'dot ' + outcomeClass} aria-hidden="true"><Circle size={7} fill="currentColor" strokeWidth={0} /></span>
      {relTime(record.ts_ms)} · {record.done} items{outcomeSuffix}
    </span>
  );
}

export function Sidebar(props: SidebarProps) {
  const { jobs, currentJobId, lastSyncByJobName, busy, reviewing, appVersion, jobsDir, onSelect, onEdit, onNew } = props;
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
        {jobs.map((job, index) => {
          const active = currentJobId === job.job_id;
          return (
          <div
            key={job.job_id}
            className={'job' + (active ? ' active' : '')}
            title={`${job.source}\n→ ${job.target}` + (job.remote ? '\n(applied by a peer syncdash over ssh)' : '')}
          >
            <button
              ref={(element) => { jobButtons.current[index] = element; }}
              type="button"
              className="job-select"
              disabled={busy}
              aria-current={active ? 'page' : undefined}
              tabIndex={active || (!currentJobId && index === 0) ? 0 : -1}
              onClick={() => { if (!active) onSelect(job); }}
              onKeyDown={(event) => {
                if (event.key === 'ArrowDown' || event.key === 'ArrowRight') {
                  event.preventDefault();
                  moveFocus(index, 1);
                } else if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') {
                  event.preventDefault();
                  moveFocus(index, -1);
                } else if (event.key === 'Home' || event.key === 'End') {
                  event.preventDefault();
                  moveFocus(index, event.key === 'Home' ? 'first' : 'last');
                }
              }}
            >
              <span className="jrow1">
                <span className="name">{job.name}</span>
                {job.remote && <span className="rbadge">ssh</span>}
                {job.rigor && job.rigor !== 'standard' && <span className="rigor">{job.rigor}</span>}
                <span className={'mode ' + job.mode}>{job.mode}</span>
              </span>
              {lastSyncByJobName[job.name]
                && <LastRunSummary record={lastSyncByJobName[job.name]} />}
            </button>
            <button
              type="button"
              className="jedit"
              aria-label={`Edit job ${job.name}`}
              title="Edit job"
              disabled={busy || reviewing}
              onClick={() => onEdit(job.name)}
            ><Pencil size={12} aria-hidden="true" /></button>
          </div>
          );
        })}
      </nav>
      <button className="btn newjob" disabled={busy || reviewing} onClick={onNew}><Plus size={13} /> New job</button>
      <div className="sidefoot">
        <span>{appVersion}</span>
        <span title={jobsDir}>{jobsDir}</span>
      </div>
    </aside>
  );
}
