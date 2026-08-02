import type { RefCallback } from 'react';
import { CornerRightUp } from 'lucide-react';
import { JOB_FORM_FIELDS, formFieldsInGroup } from '#core/domain/jobs/formSchema.ts';
import type { FormValues } from '#core/domain/jobs/formSchema.ts';
import type { JunkPresetDto } from '#core/types/generated/JunkPresetDto.ts';
import { ConfirmDialog } from '#ui/shared/components/overlays/ConfirmDialog.tsx';
import { Sheet } from '#ui/shared/components/overlays/Sheet.tsx';
import { JunkPresets } from '../JunkPresets.tsx';
import { SchemaSection } from '../SchemaSection.tsx';
import {
  JOB_FORM_GROUPS,
  type LoadedJobForm,
  type MutationKind,
  type SaveError,
} from '../../model/job-editor/jobEditorModel.ts';

interface JobEditorViewProps {
  name: string | null;
  busy: boolean;
  activeGroup: string;
  loadedForm: LoadedJobForm | null;
  mutationKind: MutationKind | null;
  saveError: SaveError | null;
  migratedFromSchema: number | null;
  schemaInspectionError: string | null;
  presetCatalog: { status: 'loading' | 'ready' | 'failed'; presets: JunkPresetDto[]; error?: string };
  deleteConfirmationOpen: boolean;
  discardConfirmationOpen: boolean;
  setFormPaneRef: RefCallback<HTMLDivElement>;
  registerSection: (group: string) => (element: HTMLElement | null) => void;
  scrollToSection: (group: string) => void;
  setFieldValue: (key: string, value: string | boolean) => void;
  pickPathForField: (key: string, kind: 'dir' | 'file') => void;
  pathFieldClassName: (key: string) => string;
  onRequestClose: () => void;
  onRequestDelete: () => void;
  onSave: () => void;
  onCopyScheduledTask: () => void;
  onDelete: () => void;
  onCancelDelete: () => void;
  onDiscard: () => void;
  onCancelDiscard: () => void;
}

export function JobEditorView({
  name,
  busy,
  activeGroup,
  loadedForm,
  mutationKind,
  saveError,
  migratedFromSchema,
  schemaInspectionError,
  presetCatalog,
  deleteConfirmationOpen,
  discardConfirmationOpen,
  setFormPaneRef,
  registerSection,
  scrollToSection,
  setFieldValue,
  pickPathForField,
  pathFieldClassName,
  onRequestClose,
  onRequestDelete,
  onSave,
  onCopyScheduledTask,
  onDelete,
  onCancelDelete,
  onDiscard,
  onCancelDiscard,
}: JobEditorViewProps) {
  return (
    <Sheet
      title={name ? `Edit job — ${name}` : 'New job'}
      width="xl"
      onClose={onRequestClose}
      footer={
        <>
          {name && <button type="button" className="btn danger" disabled={busy || mutationKind !== null} onClick={onRequestDelete}>Delete job</button>}
          {name && (
            <button
              type="button"
              className="btn"
              title="Copy the schtasks command (run it yourself in an admin terminal; this app does not register system scheduled tasks for you)"
              disabled={busy || mutationKind !== null}
              onClick={onCopyScheduledTask}
            >Copy scheduled task command</button>
          )}
          {saveError && <span className="ed-save-error" role="alert">{saveError.message}</span>}
          <span className="spacer" />
          <button type="button" className="btn" disabled={mutationKind !== null} onClick={onRequestClose}>Cancel (Esc)</button>
          <button type="button" className="btn accent" disabled={!loadedForm || mutationKind !== null || busy} onClick={onSave}>
            {mutationKind === 'save' ? 'Saving…' : 'Save'}
          </button>
        </>
      }
    >
      <>
        {migratedFromSchema !== null && (
          <div className="ed-notice">
            <span className="edn-mark"><CornerRightUp size={14} /></span>
            <span>
              This job file is at schema v{migratedFromSchema}. It opened through the one-way load
              migrations that preserve legacy filters, target roots, AutoScan settings, and evidence
              behavior in the current model. The file itself is unchanged; saving writes the complete
              current schema.
            </span>
          </div>
        )}
        {schemaInspectionError && (
          <div className="ed-notice" role="status">
            <span className="edn-mark"><CornerRightUp size={14} /></span>
            <span>
              The job opened, but its on-disk schema version could not be inspected: {schemaInspectionError}.
              Saving still validates and writes the complete current schema.
            </span>
          </div>
        )}
        {presetCatalog.status === 'failed' && (
          <div className="ed-notice" role="status">
            <span className="edn-mark"><CornerRightUp size={14} /></span>
            <span>
              Junk-preset metadata could not be loaded: {presetCatalog.error}. The exclude list remains
              fully editable by hand, but preset checkboxes are unavailable.
            </span>
          </div>
        )}
        <div className="ed-body">
          <nav className="ed-nav">
            {JOB_FORM_GROUPS.map((group) => (
              <button type="button" key={group} className={group === activeGroup ? 'on' : ''} onClick={() => scrollToSection(group)}>{group}</button>
            ))}
          </nav>
          <div className="ed-pane" ref={setFormPaneRef}>
            {loadedForm && JOB_FORM_GROUPS.map((group) => (
              <section key={group} className="ed-section" ref={registerSection(group)}>
                <h4 className="section-title ed-section-title">{group}</h4>
                <SchemaSection
                  fields={formFieldsInGroup(JOB_FORM_FIELDS, group)}
                  values={loadedForm.values}
                  set={setFieldValue}
                  onPick={pickPathForField}
                  pathClass={pathFieldClassName}
                  droppable
                  autoFocusField={name ? undefined : '__name'}
                  invalidField={saveError?.field}
                  disabledField={(fieldKey) => fieldKey === 'escalate' && loadedForm.values.evidence !== 'sampled'}
                  renderCustom={() => (
                    <JunkPresets
                      presets={presetCatalog.presets}
                      excludeText={String(loadedForm.values.exclude ?? '')}
                      onChange={(excludeText) => setFieldValue('exclude', excludeText)}
                    />
                  )}
                />
              </section>
            ))}
          </div>
        </div>

        {deleteConfirmationOpen && name && (
          <ConfirmDialog
            title={`Delete the job config '${name}'?`}
            message={
              `This removes the job file only:\n\n· ${name}.toml is deleted from the jobs directory\n` +
              '· Neither root is touched — no file on either side is read, moved or removed\n' +
              '· Past run logs and anything already in the trash are left alone'
            }
            actions={[{
              label: 'Delete the job file',
              danger: true,
              disabled: busy || mutationKind !== null,
              onConfirm: onDelete,
            }]}
            onCancel={onCancelDelete}
          />
        )}
        {discardConfirmationOpen && (
          <ConfirmDialog
            title={name ? `Discard unsaved changes to '${name}'?` : 'Discard this new job draft?'}
            message="The editor contains changes that have not been saved. Closing it now will discard this draft."
            actions={[{ label: 'Discard unsaved changes', danger: true, onConfirm: onDiscard }]}
            onCancel={onCancelDiscard}
          />
        )}
      </>
    </Sheet>
  );
}
