import { useEffect, useState } from 'react';
import { summarizePresets } from '../../core/junk';
import { pathState, usePathVerdict } from '../hooks/usePathVerdict';
import { useJunkPresets } from './JunkPresets';
import { PathVerdictBox } from './PathVerdictBox';
import type { JobDto } from '../../core/types/generated/JobDto';
import type { JobFull, JunkPresetDto } from '../../core/ipc';

interface Props {
  job: JobDto | null;
  /// Full config behind the selected job, for the pill row (JobDto carries only what the list needs)
  cfgJob: JobFull | null;
  busy: boolean;
  selTarget: number;
  pathHistory: string[];
  /// Which root input the Tauri drag handler is currently hovering, if any
  dropOn: 'source' | 'target' | null;
  onCommit: (which: 'source' | 'target', value: string) => void;
  onBrowse: (which: 'source' | 'target') => void;
  onSwap: () => void;
  onSelectTarget: (i: number) => void;
  onEditGroup: (group: string) => void;
}

const pct = (v: number) => `${Math.round(v * 100)}%`;

/// Config overview on the main screen: only the settings that change the outcome; clicking a pill jumps
/// to the matching editor group. Data comes from get_job as a full Job — JobDto is left alone so we don't
/// contend with other changes over the same struct.
/// `exclude` is the whole exclude policy now, so this pill can answer "what does this job exclude" by
/// naming the presets rather than reporting a count of opaque strings. A preset only counts as on when
/// every one of its patterns is present; a partly-present one is called out as such rather than rounded.
function filterSummary(j: JobFull, presets: JunkPresetDto[]): string {
  if (!j.exclude.length) return 'nothing excluded';
  if (!presets.length) return `${j.exclude.length} excluded`;
  const s = summarizePresets(j.exclude, presets);
  const parts = [...s.on];
  for (const p of s.partial) parts.push(`${p.label} ${p.present}/${p.total}`);
  if (s.custom) parts.push(`${s.custom} custom`);
  return parts.length ? parts.join(' · ') : `${j.exclude.length} excluded`;
}

function configPills(j: JobFull, presets: JunkPresetDto[]): [string, string, string][] {
  const pills: [string, string, string][] = [
    // [label, value, editor group]
    ['Filters', filterSummary(j, presets) + (j.include.length ? ` · ${j.include.length} allowed` : ''), 'Filters'],
    ['Conflicts', j.on_conflict + (j.on_conflict === 'copy' ? ` ≤${j.max_conflicts}` : ''), 'Behavior'],
    ['Versioning', j.versioning ? 'on (history kept under each root)' : 'off (deletes go to the local trash)', 'Behavior'],
    ['Gates', `delete ≤${pct(j.max_delete_ratio)} · free ≥${pct(j.min_free_pct)}${j.require_marker ? ' · marker required' : ''}`, 'Guardrails'],
    ['Watch', j.watch_interval_secs ? `${j.watch_interval_secs}s${j.watch_auto_apply ? ' · auto-run' : ''}` : 'off', 'Watch'],
  ];
  if (j.mode === 'sync') pills.push(['archive', j.archive ? 'configured' : 'not configured (deletes/moves cannot be attributed)', 'Basics']);
  if (j.remote_host) pills.push(['Remote', `ssh ${j.remote_host}`, 'Remote (optional)']);
  return pills;
}

/// The two roots on the main screen are **editable** (same as FFS): Enter or blur writes them back to the
/// job TOML. No "just tweak it in memory" — once the two roots in the plan header disagree with what the
/// job file says, run logs and archive refresh both point in the wrong direction.
export function PathLine(props: Props) {
  const { job, cfgJob, busy, selTarget, pathHistory, dropOn, onCommit, onBrowse, onSwap, onSelectTarget, onEditGroup } = props;

  const targets = job ? (job.targets && job.targets.length ? job.targets : [job.target]) : [];
  const targetValue = targets[selTarget] ?? '';
  const [src, setSrc] = useState(job?.source ?? '');
  const [tgt, setTgt] = useState(targetValue);

  // Re-seed whenever the job (or the selected target of a 1:N job) changes underneath the box
  useEffect(() => { setSrc(job?.source ?? ''); }, [job?.name, job?.source]);
  useEffect(() => { setTgt(targetValue); }, [job?.name, targetValue]);

  const verdict = usePathVerdict(src, tgt, !!job);
  const presets = useJunkPresets();

  const cls = (which: 'source' | 'target') => {
    const base = pathState(which === 'source' ? verdict?.source : verdict?.target, which === 'source' ? src : tgt);
    return ['mono', base, dropOn === which ? 'dropon' : ''].filter(Boolean).join(' ');
  };

  return (
    <div id="pathline" className="pathline">
      <div className="prow">
        <span className="plabel">source</span>
        <input
          type="text"
          className={cls('source')}
          data-drop="1"
          data-root="source"
          list="sd-paths"
          spellCheck={false}
          placeholder="Select a job, then edit here"
          disabled={!job || busy}
          title={src}
          value={src}
          onChange={(e) => setSrc(e.target.value)}
          // change fires only on Enter or blur — nothing is written to disk while typing
          onBlur={() => onCommit('source', src)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
            if (e.key === 'Escape') { setSrc(job?.source ?? ''); (e.target as HTMLInputElement).blur(); }
          }}
        />
        <button className="pbtn" title="Browse…" disabled={!job || busy} onClick={() => onBrowse('source')}>📁</button>
        <button
          className="pbtn"
          title={job ? `Swap: ${job.source} ⇄ ${job.target} (written back to the job file)` : 'Swap source / target'}
          disabled={!job || busy}
          onClick={onSwap}
        >⇄</button>
        <span className="plabel">target</span>
        <input
          type="text"
          className={cls('target')}
          data-drop="1"
          data-root="target"
          list="sd-paths"
          spellCheck={false}
          disabled={!job || busy}
          title={tgt}
          value={tgt}
          onChange={(e) => setTgt(e.target.value)}
          onBlur={() => onCommit('target', tgt)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
            if (e.key === 'Escape') { setTgt(targetValue); (e.target as HTMLInputElement).blur(); }
          }}
        />
        <button className="pbtn" title="Browse…" disabled={!job || busy} onClick={() => onBrowse('target')}>📁</button>
        {targets.length > 1 && (
          <select
            id="target-sel"
            title="Multi-target job: pick the target to work on"
            value={selTarget}
            onChange={(e) => onSelectTarget(Number(e.target.value) || 0)}
          >
            {targets.map((t, i) => (
              <option key={i} value={i}>target {i + 1}/{targets.length}: {t}</option>
            ))}
          </select>
        )}
      </div>

      {/* Path history is a native datalist (keyboard-friendly, no custom popup layer); it lives here so
          both root boxes and the editor's path fields can reference it by id. */}
      <datalist id="sd-paths">
        {pathHistory.map((p) => <option key={p} value={p} />)}
      </datalist>

      <PathVerdictBox verdict={job ? verdict : null} className="pwarn" />

      <div className="cfgline">
        {cfgJob && configPills(cfgJob, presets).map(([k, v, g]) => (
          <button
            key={k}
            className="cfgpill"
            title={`Open the '${g}' group in the editor`}
            onClick={() => onEditGroup(g)}
          >
            <span className="ck">{k}</span><span className="cv">{v}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
