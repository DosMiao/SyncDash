import { useEffect, useState } from 'react';
import { ArrowLeftRight, FolderOpen } from 'lucide-react';
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
  /// Registers this row as a drop region: the drag handler hit-tests the inputs inside it
  scopeRef: (el: HTMLElement | null) => void;
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

interface Pill { key: string; value: string; group: string; title: string }

/// A pill states the setting and its value; what the value *means* is in the tooltip. These sit on
/// the main screen, where a parenthetical explanation costs a column of the diff table to say
/// something you only need once.
function configPills(j: JobFull, presets: JunkPresetDto[]): Pill[] {
  const pills: Pill[] = [
    {
      key: 'Filters',
      value: filterSummary(j, presets) + (j.include.length ? ` · ${j.include.length} allowed` : ''),
      group: 'Filters',
      title: 'Which junk presets are on, plus any hand-written rules. Whatever the filter removes is counted in the status bar.',
    },
    {
      key: 'Conflicts',
      value: j.on_conflict + (j.on_conflict === 'copy' ? ` ≤${j.max_conflicts}` : ''),
      group: 'Behavior',
      title: 'What happens when both sides changed since the last run.\nreport = list them and change nothing · copy = keep both sides · newer = the newer file wins',
    },
    {
      key: 'Versioning',
      value: j.versioning ? 'on' : 'off',
      group: 'Behavior',
      title: 'On: replaced and deleted files are kept under .version_syncDash in each root.\nOff: deletes go to the local trash instead.',
    },
    {
      key: 'Gates',
      value: `≤${pct(j.max_delete_ratio)} del · ≥${pct(j.min_free_pct)} free${j.require_marker ? ' · marker' : ''}`,
      group: 'Guardrails',
      title: `A run is blocked if it would delete more than ${pct(j.max_delete_ratio)} of the target, or if free disk is under ${pct(j.min_free_pct)}.`
        + (j.require_marker ? '\nBoth roots must also carry a .syncdash-root marker.' : ''),
    },
    {
      key: 'AutoScan',
      value: j.watch_interval_secs ? `${j.watch_interval_secs}s${j.watch_auto_apply ? ' · auto' : ''}` : 'off',
      group: 'AutoScan',
      title: j.watch_interval_secs
        ? `Compares every ${j.watch_interval_secs}s${j.watch_auto_apply ? ' and runs the result automatically' : ' and waits for you to review'}.`
        : 'No scheduled comparison.',
    },
  ];
  if (j.mode === 'sync') {
    pills.push({
      key: 'Archive',
      value: j.archive ? 'set' : 'none',
      group: 'Basics',
      title: j.archive
        ? `Last-run table: ${j.archive}`
        : 'Without an archive, sync mode cannot tell a delete from a file that was never there — deletes and moves are not attributed.',
    });
  }
  // The link used to live in three job fields; it is in the target phrase now, which the box above
  // already shows in full. So the pill reports only what the phrase does not make obvious: that
  // this job pushes to a peer, and whether it declared a mount to pull back through.
  if (j.target?.startsWith('peer://')) {
    const mounted = /\|mount=/.test(j.target);
    pills.push({
      key: 'Peer',
      value: mounted ? 'push + pull' : 'push only',
      group: 'Remote',
      title: mounted
        ? 'The far side runs its own syncdash and applies what this side packs. Source-side (pull) ops write through the declared |mount= path.'
        : 'The far side runs its own syncdash and applies what this side packs. No |mount= is declared, so source-side (pull) ops are skipped — add |mount=<path serving the same tree> to enable them.',
    });
  }
  return pills;
}

/// The two roots on the main screen are **editable** (same as FFS): Enter or blur writes them back to the
/// job TOML. No "just tweak it in memory" — once the two roots in the plan header disagree with what the
/// job file says, run logs and archive refresh both point in the wrong direction.
export function PathLine(props: Props) {
  const { job, cfgJob, busy, selTarget, pathHistory, dropOn, scopeRef, onCommit, onBrowse, onSwap, onSelectTarget, onEditGroup } = props;

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
    <div className="pathline" ref={scopeRef}>
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
        <button className="pbtn" title="Browse…" disabled={!job || busy} onClick={() => onBrowse('source')}>
          <FolderOpen size={13} />
        </button>
        <button
          className="pbtn"
          title={job ? `Swap: ${job.source} ⇄ ${job.target} (written back to the job file)` : 'Swap source / target'}
          disabled={!job || busy}
          onClick={onSwap}
        ><ArrowLeftRight size={13} /></button>
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
        <button className="pbtn" title="Browse…" disabled={!job || busy} onClick={() => onBrowse('target')}>
          <FolderOpen size={13} />
        </button>
        {targets.length > 1 && (
          <select
            className="target-sel"
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
        {cfgJob && configPills(cfgJob, presets).map((p) => (
          <button
            key={p.key}
            className="cfgpill"
            title={`${p.title}\n\nClick to edit — opens ${p.group}.`}
            onClick={() => onEditGroup(p.group)}
          >
            <span className="ck">{p.key}</span><span className="cv">{p.value}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
