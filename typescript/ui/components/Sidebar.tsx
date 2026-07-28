import { Circle, Pencil, Plus } from 'lucide-react';
import { relTime } from '../../core/format';
import type { JobDto } from '../../core/types/generated/JobDto';
import type { RunRecord } from '../../core/types/generated/RunRecord';

interface Props {
  jobs: JobDto[];
  currentName: string | null;
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
    <div className={'jrow2' + (stale ? ' stale' : '')}>
      {/* Filled rather than outlined: at 7px an outlined ring reads as a smudge, and this dot is
          carrying the whole outcome of the last run */}
      <span className={'dot ' + dot}><Circle size={7} fill="currentColor" strokeWidth={0} /></span>
      {relTime(r.ts_ms)} · {r.done} items{note}
    </div>
  );
}

export function Sidebar(props: Props) {
  const { jobs, currentName, lastMap, busy, appVersion, jobsDir, onSelect, onEdit, onNew } = props;

  return (
    <aside className="sidebar">
      <div className="brand" data-tauri-drag-region>Sync<span>Dash</span></div>
      <div className="joblist">
        {jobs.map((j) => (
          <div
            key={j.name}
            className={'job' + (currentName === j.name ? ' active' : '')}
            title={`${j.source}\n→ ${j.target}` + (j.remote ? `\nssh:${j.remote_host ?? ''}` : '')}
            onClick={() => { if (!busy) onSelect(j); }}
          >
            <div className="jrow1">
              <span className="name">{j.name}</span>
              {j.remote && <span className="rbadge">ssh</span>}
              {j.rigor && j.rigor !== 'standard' && <span className="rigor">{j.rigor}</span>}
              <span className={'mode ' + j.mode}>{j.mode}</span>
              <button
                className="jedit"
                title="Edit job"
                onClick={(e) => { e.stopPropagation(); onEdit(j.name); }}
              ><Pencil size={12} /></button>
            </div>
            {lastMap[j.name] && <LastRun r={lastMap[j.name]} />}
          </div>
        ))}
      </div>
      <button className="btn newjob" onClick={onNew}><Plus size={13} /> New job</button>
      <div className="sidefoot">
        <span>{appVersion}</span>
        <span>{jobsDir}</span>
      </div>
    </aside>
  );
}
