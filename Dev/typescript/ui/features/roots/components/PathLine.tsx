import { ArrowLeftRight, FolderOpen } from 'lucide-react';
import { summarizePresets } from '#core/domain/jobs/junk.ts';
import { pathState, usePathVerdict } from '#ui/features/roots/hooks/usePathVerdict.ts';
import { rootEditKeyAction } from '#core/application/safety/executionSafety.ts';
import { rootDraftIsDirty } from '#core/application/jobs/rootEditor.ts';
import { useJunkPresetCatalog } from '#ui/features/jobs/hooks/useJunkPresetCatalog.ts';
import { PathVerdictBox } from './PathVerdictBox';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { Job as JobFull } from '#core/types/generated/Job.ts';
import type { JunkPresetDto } from '#core/types/generated/JunkPresetDto.ts';
import type { RootEditorWorkspace, RootField } from '#core/application/jobs/rootEditor.ts';

interface PathLineProps {
  job: JobDto | null;
  rootEditor: RootEditorWorkspace | null;
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
  onDraftChange: (field: RootField, value: string) => void;
  onSave: (field: RootField) => void;
  onRevert: (field: RootField) => void;
  onAcceptConflict: (field: RootField) => void;
  onBrowse: (field: RootField) => void;
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
      title: `Review & Apply colors a category red once it touches more than ${formatPercentage(job.max_delete_ratio)} of the target. A run is blocked only if free disk is under ${formatPercentage(job.min_free_pct)}.`
        + (job.require_marker ? '\nBoth roots must also carry a .syncdash-root marker.' : ''),
    },
    {
      key: 'AutoScan',
      value: job.autoscan_interval_secs ? `${job.autoscan_interval_secs}s${job.autoscan_auto_apply ? ' · auto' : ''}` : 'off',
      group: 'AutoScan',
      title: job.autoscan_interval_secs
        ? `Compares every ${job.autoscan_interval_secs}s${job.autoscan_auto_apply ? ' and runs the result automatically' : ' and waits for you to review'}.`
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
  const peerTarget = job.targets.find((target) => target.startsWith('peer://'));
  if (peerTarget) {
    const mounted = /\|mount=/.test(peerTarget);
    pills.push({
      key: 'Peer',
      value: mounted ? 'push + pull' : 'push only',
      group: 'Basics',
      title: mounted
        ? 'The far side runs its own syncdash and applies what this side packs. Source-side (pull) ops write through the declared |mount= path.'
        : 'The far side runs its own syncdash and applies what this side packs. No |mount= is declared, so source-side (pull) ops are skipped — add |mount=<path serving the same tree> to enable them.',
    });
  }
  return pills;
}

export function PathLine(props: PathLineProps) {
  const {
    job, rootEditor, jobConfiguration, busy, reviewing, selectedTargetIndex, pathHistory,
    dropTargetKey, scopeRef, onDraftChange, onSave, onRevert, onAcceptConflict, onBrowse,
    onSwap, onSelectTarget, onEditGroup,
  } = props;
  const saving = rootEditor?.save.status === 'saving';
  const mutationBusy = busy || reviewing || saving;

  const targets = job?.targets ?? [];
  const targetValue = targets[selectedTargetIndex] ?? '';
  const sourceDraft = rootEditor?.draft.source ?? '';
  const targetDraft = rootEditor?.draft.target ?? '';
  const pathInspection = usePathVerdict(sourceDraft, targetDraft, !!job && !!rootEditor);
  const presetCatalog = useJunkPresetCatalog();
  const sourceDirty = rootEditor ? rootDraftIsDirty(rootEditor, 'source') : false;
  const targetDirty = rootEditor ? rootDraftIsDirty(rootEditor, 'target') : false;
  const hasDraft = sourceDirty || targetDirty;

  const inputClassName = (which: RootField) => {
    const base = pathState(
      pathInspection,
      which,
      which === 'source' ? sourceDraft : targetDraft,
    );
    return [
      'mono',
      base,
      dropTargetKey === which ? 'dropon' : '',
      rootEditor?.conflicts[which] ? 'root-conflicted' : '',
    ].filter(Boolean).join(' ');
  };

  const fieldActions = (field: RootField) => {
    const dirty = field === 'source' ? sourceDirty : targetDirty;
    if (!dirty || !rootEditor) return null;
    const conflict = rootEditor.conflicts[field];
    return (
      <span className="root-edit-actions">
        {conflict ? (
          <button
            type="button"
            className="pbtn root-action"
            disabled={mutationBusy}
            title={`The saved value changed from ${conflict.previousCommittedValue} to ${conflict.currentCommittedValue}. Keep this draft and allow a new save.`}
            onClick={() => onAcceptConflict(field)}
          >Keep draft</button>
        ) : (
          <button
            type="button"
            className="pbtn root-action save"
            disabled={mutationBusy || !(field === 'source' ? sourceDraft : targetDraft).trim()}
            title={`Save the ${field} root (Enter)`}
            onClick={() => onSave(field)}
          >Save</button>
        )}
        <button
          type="button"
          className="pbtn root-action"
          disabled={mutationBusy}
          title={`Restore the saved ${field} root (Escape)`}
          onClick={() => onRevert(field)}
        >Cancel</button>
      </span>
    );
  };

  const handleRootKey = (event: React.KeyboardEvent<HTMLInputElement>, field: RootField) => {
    const action = rootEditKeyAction(event.key);
    if (!action) return;
    event.preventDefault();
    if (action === 'revert') onRevert(field);
    else onSave(field);
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
          onChange={(event) => onDraftChange('source', event.target.value)}
          onKeyDown={(event) => handleRootKey(event, 'source')}
        />
        <button type="button" className="pbtn" title="Browse…" disabled={!job || mutationBusy} onClick={() => onBrowse('source')}>
          <FolderOpen size={13} />
        </button>
        <button
          type="button"
          className="pbtn"
          title={hasDraft
            ? 'Save or cancel both root drafts before swapping'
            : job ? `Swap: ${job.source} ⇄ ${targetValue} (written back to the job file)` : 'Swap source / target'}
          disabled={!job || mutationBusy || hasDraft}
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
          onChange={(event) => onDraftChange('target', event.target.value)}
          onKeyDown={(event) => handleRootKey(event, 'target')}
        />
        <button type="button" className="pbtn" title="Browse…" disabled={!job || mutationBusy} onClick={() => onBrowse('target')}>
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

      {hasDraft && (
        <div className="root-draft-row">
          <span>Unsaved root draft</span>
          {sourceDirty && <><strong>source</strong>{fieldActions('source')}</>}
          {targetDirty && <><strong>target</strong>{fieldActions('target')}</>}
        </div>
      )}

      {/* Path history is a native datalist (keyboard-friendly, no custom popup layer); it lives here so
          both root boxes and the editor's path fields can reference it by id. */}
      <datalist id="sd-paths">
        {pathHistory.map((path) => <option key={path} value={path} />)}
      </datalist>

      <PathVerdictBox inspection={pathInspection} className="pwarn" />
      {rootEditor?.save.status === 'failed' && (
        <div className="root-save-error" role="alert">
          Could not save {rootEditor.save.field}: {rootEditor.save.error}. Your draft is still here.
        </div>
      )}

      <div className="cfgline">
        {jobConfiguration && configPills(jobConfiguration, presetCatalog.presets).map((pill) => (
          <button
            key={pill.key}
            type="button"
            className="cfgpill"
            title={`${pill.title}\n\nClick to edit — opens ${pill.group}.`}
            disabled={mutationBusy || hasDraft}
            onClick={() => onEditGroup(pill.group)}
          >
            <span className="ck">{pill.key}</span><span className="cv">{pill.value}</span>
          </button>
        ))}
        {presetCatalog.status === 'failed' && (
          <span
            className="cfgpill"
            role="status"
            title={`Junk-preset metadata could not be loaded: ${presetCatalog.error}. Filter counts still come from the saved job.`}
          >
            <span className="ck">Filters</span><span className="cv">preset metadata unavailable</span>
          </span>
        )}
      </div>
    </div>
  );
}
