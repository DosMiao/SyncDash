import { ArrowLeftRight, FolderOpen } from 'lucide-react';
import { configPills } from '#ui/features/roots/model/configPills.ts';
import { pathState, usePathVerdict } from '#ui/features/roots/hooks/usePathVerdict.ts';
import { rootEditKeyAction } from '#core/application/safety/executionSafety.ts';
import { rootDraftIsDirty } from '#core/application/jobs/rootEditor.ts';
import { useJunkPresetCatalog } from '#ui/features/jobs/hooks/useJunkPresetCatalog.ts';
import { PathVerdictBox } from './PathVerdictBox';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { Job as JobFull } from '#core/types/generated/Job.ts';
import type { PeerLinkDto } from '#core/types/generated/PeerLinkDto.ts';
import type { RootEditorWorkspace, RootField } from '#core/application/jobs/rootEditor.ts';

interface PathLineProps {
  job: JobDto | null;
  rootEditor: RootEditorWorkspace | null;
  /// Full config behind the selected job, for the pill row (JobDto carries only what the list needs)
  jobConfiguration: JobFull | null;
  /// The peer verdict the engine derived for that same job, from the same `get_job` payload as
  /// `jobConfiguration`, so the pill row never mixes two independently refreshed snapshots.
  peerLink: PeerLinkDto | null;
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

export function PathLine(props: PathLineProps) {
  const {
    job, rootEditor, jobConfiguration, peerLink, busy, reviewing, selectedTargetIndex, pathHistory,
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
        {jobConfiguration && configPills(jobConfiguration, peerLink, presetCatalog.presets).map((pill) => (
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
