import { useRef } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent } from 'react';
import { Circle, Pencil, Plus } from 'lucide-react';
import { formatRelativeTimestamp } from '#core/shared/format.ts';
import { isRovingFocusKey, rovingFocusIndex } from '#ui/shared/components/a11y.ts';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { RunRecord } from '#core/types/generated/RunRecord.ts';

interface SidebarProps {
  jobs: JobDto[];
  currentJobId: string | null;
  latestRunByJobId: Record<string, RunRecord>;
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
      {formatRelativeTimestamp(record.ts_ms)} · {record.done} items{outcomeSuffix}
    </span>
  );
}

export function Sidebar(props: SidebarProps) {
  const {
    jobs,
    currentJobId,
    latestRunByJobId,
    busy,
    reviewing,
    appVersion,
    jobsDir,
    onSelect,
    onEdit,
    onNew,
  } = props;
  const jobButtons = useRef<Array<HTMLButtonElement | null>>([]);

  const moveFocus = (event: ReactKeyboardEvent<HTMLButtonElement>, from: number) => {
    if (!isRovingFocusKey(event.key)) return;
    const next = rovingFocusIndex(event.key, from, jobs.length);
    if (next === null) return;
    event.preventDefault();
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
            title={`${job.source}\n→ ${job.targets[0] ?? '(missing target)'}` + (job.is_peer_job ? '\n(applied by a peer SyncDash over SSH)' : '')}
          >
            <button
              ref={(element) => { jobButtons.current[index] = element; }}
              type="button"
              className="job-select"
              disabled={busy}
              aria-current={active ? 'page' : undefined}
              tabIndex={active || (!currentJobId && index === 0) ? 0 : -1}
              onClick={() => { if (!active) onSelect(job); }}
              onKeyDown={(event) => moveFocus(event, index)}
            >
              <span className="jrow1">
                <span className="name">{job.name}</span>
                {job.is_peer_job && <span className="rbadge">peer</span>}
                {job.rigor && job.rigor !== 'standard' && <span className="rigor">{job.rigor}</span>}
                <span className={'mode ' + job.mode}>{job.mode}</span>
              </span>
              {latestRunByJobId[job.job_id]
                && <LastRunSummary record={latestRunByJobId[job.job_id]} />}
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
      <button type="button" className="btn newjob" disabled={busy || reviewing} onClick={onNew}><Plus size={13} /> New job</button>
      <div className="sidefoot">
        <span>{appVersion}</span>
        <span title={jobsDir}>{jobsDir}</span>
      </div>
    </aside>
  );
}
