import { useEffect, useRef, useState } from 'react';
import { ArrowLeftRight, FolderOpen } from 'lucide-react';
import { summarizePresets } from '../../core/junk';
import { pathState, usePathVerdict } from '../hooks/usePathVerdict';
import { rootEditKeyAction } from '../state/execution-safety';
import { useJunkPresets } from './JunkPresets';
import { PathVerdictBox } from './PathVerdictBox';
import type { JobDto } from '../../core/types/generated/JobDto';
import type { JobFull, JunkPresetDto } from '../../core/ipc';

interface PathLineProps {
  job: JobDto | null;
  /// Full config behind the selected job, for the pill row (JobDto carries only what the list needs)
  jobConfiguration: JobFull | null;
  busy: boolean;
  /// Keep target navigation available so a pending review can be abandoned, while preventing root
  /// and config mutations from racing the review request.
  reviewing: boolean;
  selectedTargetIndex: number;
  pathHistory: string[];
  /// Which root input the Tauri drag handler is currently hovering, if any
  dropTargetKey: 'source' | 'target' | null;
  /// Registers this row as a drop region: the drag handler hit-tests the inputs inside it
  scopeRef: (element: HTMLElement | null) => void;
  onCommit: (which: 'source' | 'target', value: string) => void;
  onBrowse: (which: 'source' | 'target') => void;
  onSwap: () => void;
  onSelectTarget: (index: number) => void;
  onEditGroup: (group: string) => void;
}

const formatPercentage = (value: number) => `${Math.round(value * 100)}%`;

/// Config overview on the main screen: only the settings that change the outcome; clicking a pill jumps
/// to the matching editor group. Data comes from get_job as a full Job — JobDto is left alone so we don't
/// contend with other changes over the same struct.
/// `exclude` is the whole exclude policy now, so this pill can answer "what does this job exclude" by
/// naming the presets rather than reporting a count of opaque strings. A preset only counts as on when
/// every one of its patterns is present; a partly-present one is called out as such rather than rounded.
function filterSummary(job: JobFull, presets: JunkPresetDto[]): string {
  if (!job.exclude.length) return 'nothing excluded';
  if (!presets.length) return `${job.exclude.length} excluded`;
  const summary = summarizePresets(job.exclude, presets);
  const parts = [...summary.on];
  for (const partial of summary.partial) {
    parts.push(`${partial.label} ${partial.present}/${partial.total}`);
  }
  if (summary.custom) parts.push(`${summary.custom} custom`);
  return parts.length ? parts.join(' · ') : `${job.exclude.length} excluded`;
}

interface Pill { key: string; value: string; group: string; title: string }

/// A pill states the setting and its value; what the value *means* is in the tooltip. These sit on
/// the main screen, where a parenthetical explanation costs a column of the diff table to say
/// something you only need once.
function configPills(job: JobFull, presets: JunkPresetDto[]): Pill[] {
  const pills: Pill[] = [
    {
      key: 'Filters',
      value: filterSummary(job, presets) + (job.include.length ? ` · ${job.include.length} allowed` : ''),
      group: 'Filters',
      title: 'Which junk presets are on, plus any hand-written rules. Whatever the filter removes is counted in the status bar.',
    },
    {
      key: 'Conflicts',
      value: job.on_conflict + (job.on_conflict === 'copy' ? ` ≤${job.max_conflicts}` : ''),
      group: 'Behavior',
      title: 'What happens when both sides changed since the last run.\nreport = list them and change nothing · copy = keep both sides · newer = the newer file wins',
    },
    {
      key: 'Versioning',
      value: job.versioning ? 'on' : 'off',
      group: 'Behavior',
      title: 'On: replaced and deleted files are kept under .version_syncDash in each root.\nOff: deletes go to the local trash instead.',
    },
    {
      key: 'Gates',
      value: `≤${formatPercentage(job.max_delete_ratio)} del · ≥${formatPercentage(job.min_free_pct)} free${job.require_marker ? ' · marker' : ''}`,
      group: 'Guardrails',
      title: `A run is blocked if it would delete more than ${formatPercentage(job.max_delete_ratio)} of the target, or if free disk is under ${formatPercentage(job.min_free_pct)}.`
        + (job.require_marker ? '\nBoth roots must also carry a .syncdash-root marker.' : ''),
    },
    {
      key: 'AutoScan',
      value: job.watch_interval_secs ? `${job.watch_interval_secs}s${job.watch_auto_apply ? ' · auto' : ''}` : 'off',
      group: 'AutoScan',
      title: job.watch_interval_secs
        ? `Compares every ${job.watch_interval_secs}s${job.watch_auto_apply ? ' and runs the result automatically' : ' and waits for you to review'}.`
        : 'No scheduled comparison.',
    },
  ];
  if (job.mode === 'sync') {
    pills.push({
      key: 'Archive',
      value: job.archive ? 'set' : 'none',
      group: 'Basics',
      title: job.archive
        ? `Last-run table: ${job.archive}`
        : 'Without an archive, sync mode cannot tell a delete from a file that was never there — deletes and moves are not attributed.',
    });
  }
  // The link used to live in three job fields; it is in the target phrase now, which the box above
  // already shows in full. So the pill reports only what the phrase does not make obvious: that
  // this job pushes to a peer, and whether it declared a mount to pull back through.
  if (job.target?.startsWith('peer://')) {
    const mounted = /\|mount=/.test(job.target);
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
export function PathLine(props: PathLineProps) {
  const { job, jobConfiguration, busy, reviewing, selectedTargetIndex, pathHistory, dropTargetKey, scopeRef, onCommit, onBrowse, onSwap, onSelectTarget, onEditGroup } = props;
  const mutationBusy = busy || reviewing;

  const targets = job ? (job.targets && job.targets.length ? job.targets : [job.target]) : [];
  const targetValue = targets[selectedTargetIndex] ?? '';
  const [sourceDraft, setSourceDraft] = useState(job?.source ?? '');
  const [targetDraft, setTargetDraft] = useState(targetValue);
  const suppressSourceBlur = useRef(false);
  const suppressTargetBlur = useRef(false);

  // Re-seed whenever the job (or the selected target of a 1:N job) changes underneath the box
  useEffect(() => { setSourceDraft(job?.source ?? ''); }, [job?.name, job?.source]);
  useEffect(() => { setTargetDraft(targetValue); }, [job?.name, targetValue]);

  const verdict = usePathVerdict(sourceDraft, targetDraft, !!job);
  const presets = useJunkPresets();

  const inputClassName = (which: 'source' | 'target') => {
    const base = pathState(
      which === 'source' ? verdict?.source : verdict?.target,
      which === 'source' ? sourceDraft : targetDraft,
    );
    return ['mono', base, dropTargetKey === which ? 'dropon' : ''].filter(Boolean).join(' ');
  };

  return (
    <div className="pathline" ref={scopeRef}>
      <div className="prow">
        <span className="plabel">source</span>
        <input
          type="text"
          className={inputClassName('source')}
          data-drop="1"
          data-root="source"
          list="sd-paths"
          spellCheck={false}
          placeholder="Select a job, then edit here"
          disabled={!job || mutationBusy}
          title={sourceDraft}
          value={sourceDraft}
          onChange={(event) => setSourceDraft(event.target.value)}
          // change fires only on Enter or blur — nothing is written to disk while typing
          onBlur={() => {
            if (suppressSourceBlur.current) { suppressSourceBlur.current = false; return; }
            onCommit('source', sourceDraft);
          }}
          onKeyDown={(event) => {
            const action = rootEditKeyAction(event.key);
            if (!action) return;
            event.preventDefault();
            if (action === 'revert') {
              suppressSourceBlur.current = true;
              setSourceDraft(job?.source ?? '');
            }
            (event.target as HTMLInputElement).blur();
          }}
        />
        <button className="pbtn" title="Browse…" disabled={!job || mutationBusy} onClick={() => onBrowse('source')}>
          <FolderOpen size={13} />
        </button>
        <button
          className="pbtn"
          title={job ? `Swap: ${job.source} ⇄ ${targetValue} (written back to the job file)` : 'Swap source / target'}
          disabled={!job || mutationBusy}
          onClick={onSwap}
        ><ArrowLeftRight size={13} /></button>
        <span className="plabel">target</span>
        <input
          type="text"
          className={inputClassName('target')}
          data-drop="1"
          data-root="target"
          list="sd-paths"
          spellCheck={false}
          disabled={!job || mutationBusy}
          title={targetDraft}
          value={targetDraft}
          onChange={(event) => setTargetDraft(event.target.value)}
          onBlur={() => {
            if (suppressTargetBlur.current) { suppressTargetBlur.current = false; return; }
            onCommit('target', targetDraft);
          }}
          onKeyDown={(event) => {
            const action = rootEditKeyAction(event.key);
            if (!action) return;
            event.preventDefault();
            if (action === 'revert') {
              suppressTargetBlur.current = true;
              setTargetDraft(targetValue);
            }
            (event.target as HTMLInputElement).blur();
          }}
        />
        <button className="pbtn" title="Browse…" disabled={!job || mutationBusy} onClick={() => onBrowse('target')}>
          <FolderOpen size={13} />
        </button>
        {targets.length > 1 && (
          <select
            className="target-sel"
            title="Multi-target job: pick the target to work on"
            value={selectedTargetIndex}
            disabled={busy}
            onChange={(event) => onSelectTarget(Number(event.target.value) || 0)}
          >
            {targets.map((target, index) => (
              <option key={index} value={index}>target {index + 1}/{targets.length}: {target}</option>
            ))}
          </select>
        )}
      </div>

      {/* Path history is a native datalist (keyboard-friendly, no custom popup layer); it lives here so
          both root boxes and the editor's path fields can reference it by id. */}
      <datalist id="sd-paths">
        {pathHistory.map((path) => <option key={path} value={path} />)}
      </datalist>

      <PathVerdictBox verdict={job ? verdict : null} className="pwarn" />

      <div className="cfgline">
        {jobConfiguration && configPills(jobConfiguration, presets).map((pill) => (
          <button
            key={pill.key}
            className="cfgpill"
            title={`${pill.title}\n\nClick to edit — opens ${pill.group}.`}
            disabled={mutationBusy}
            onClick={() => onEditGroup(pill.group)}
          >
            <span className="ck">{pill.key}</span><span className="cv">{pill.value}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
