import { useCallback, useEffect, useRef, useState } from 'react';
import { CornerRightUp } from 'lucide-react';
import {
  JOB_FORM_FIELDS, RIGOR_PRESETS, applyRigorPresetDefaults, detectRigor, formFieldsInGroup, formToJob,
  formGroupNames, jobToForm, windowsScheduledTaskCommand,
} from '../../core/formSchema';
import { defaultJob, deleteJob, getJob, jobFileSchema, pickDirectory, pickSavePath, saveJob } from '../../core/ipc';
import { JunkPresets, useJunkPresetCatalog } from './JunkPresets';
import { SchemaSection } from './SchemaSection';
import { ConfirmDialog, Sheet } from './ui';
import { useScrollSpy } from '../hooks/useScrollSpy';
import type { FormValues } from '../../core/formSchema';
import type { JobDeleteDto, JobFull, JobSaveDto } from '../../core/ipc';
import type { StatusAppearance } from '../hooks/useStatus';

export interface JobEditorApi { setField: (key: string, value: string) => void }

const JOB_FORM_GROUPS = formGroupNames(JOB_FORM_FIELDS);

interface JobEditorProps {
  /// `null` creates a new job from the backend-owned defaults.
  name: string | null;
  /// Section to open on. The config pills on the main screen each name the section that owns the
  /// setting they show, so clicking one lands you on it rather than scrolling you near it.
  focusGroup?: string;
  dropTargetKey: string | null;
  /// Registers the form as the drop region. While the editor is open it takes precedence over the
  /// main screen's two roots, and unmounting clears it — which is the whole reason it is a callback ref.
  scopeRef: (el: HTMLElement | null) => void;
  apiRef: React.RefObject<JobEditorApi | null>;
  busy: boolean;
  onClose: () => void;
  onSaved: (
    saved: JobSaveDto,
    job: JobFull,
    original: { jobId: string; name: string; configRevision: string } | null,
  ) => void;
  onDeleted: (deleted: JobDeleteDto) => void;
  onMutationConflict: (
    name: string,
    original: { jobId: string; name: string; configRevision: string } | null,
  ) => Promise<void>;
  onStatus: (message: string, appearance?: StatusAppearance) => void;
}

/// `base` retains fields outside the visible schema so a save cannot silently reset them.
interface LoadedJobForm {
  base: JobFull;
  values: FormValues;
  initialValues: FormValues;
  originalName: string | null;
  jobId: string | null;
  configRevision: string | null;
}

interface SaveError { message: string; field?: string }
type MutationKind = 'save' | 'delete';

function sameFormValues(left: FormValues, right: FormValues): boolean {
  const keys = new Set([...Object.keys(left), ...Object.keys(right)]);
  return [...keys].every((key) => left[key] === right[key]);
}

export function JobEditor(props: JobEditorProps) {
  const {
    name, focusGroup, dropTargetKey, scopeRef, apiRef, busy, onClose, onSaved, onDeleted,
    onMutationConflict, onStatus,
  } = props;
  const [loadedForm, setLoadedForm] = useState<LoadedJobForm | null>(null);
  const [migratedFromSchema, setMigratedFromSchema] = useState<number | null>(null);
  const [schemaInspectionError, setSchemaInspectionError] = useState<string | null>(null);
  const [deleteConfirmationOpen, setDeleteConfirmationOpen] = useState(false);
  const [discardConfirmationOpen, setDiscardConfirmationOpen] = useState(false);
  const [saveError, setSaveError] = useState<SaveError | null>(null);
  const mutationInFlight = useRef(false);
  const mounted = useRef(false);
  const loadRequestId = useRef(0);
  const pickerRequest = useRef<{ id: number; field: string; fieldRevision: number } | null>(null);
  const nextPickerRequestId = useRef(0);
  const validationFocusFrame = useRef<number | null>(null);
  const fieldRevisions = useRef(new Map<string, number>());
  const onCloseRef = useRef(onClose);
  const onStatusRef = useRef(onStatus);
  onCloseRef.current = onClose;
  onStatusRef.current = onStatus;
  const [mutationKind, setMutationKind] = useState<MutationKind | null>(null);
  const [formPane, setFormPane] = useState<HTMLDivElement | null>(null);
  const { active, register, scrollTo } = useScrollSpy(formPane, JOB_FORM_GROUPS);
  const formDirty = !!loadedForm && !sameFormValues(loadedForm.values, loadedForm.initialValues);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      loadRequestId.current += 1;
      pickerRequest.current = null;
      if (validationFocusFrame.current !== null) {
        cancelAnimationFrame(validationFocusFrame.current);
        validationFocusFrame.current = null;
      }
    };
  }, []);

  // The pane is two things at once: the scrollspy's container and the drag-drop region. One stable
  // callback, not an inline arrow — React detaches and reattaches a ref whose identity changed, so
  // an inline one would null out both on every render.
  const setFormPaneRef = useCallback((element: HTMLDivElement | null) => {
    setFormPane(element);
    scopeRef(element);
  }, [scopeRef]);

  const setFieldValue = useCallback((key: string, value: string | boolean) => {
    fieldRevisions.current.set(key, (fieldRevisions.current.get(key) ?? 0) + 1);
    setSaveError(null);
    setLoadedForm((currentForm) => {
      if (!currentForm) return currentForm;
      const nextValues = { ...currentForm.values, [key]: value };
      if (key === 'rigor') {
        const preset = RIGOR_PRESETS[String(value)];
        if (preset) {
          nextValues.evidence = preset.evidence;
          nextValues.use_cache = preset.use_cache;
          nextValues.escalate = preset.escalate;
          nextValues.verify_writes = preset.verify_writes;
        }
      } else if (key === 'evidence' || key === 'use_cache' || key === 'escalate' || key === 'verify_writes') {
        nextValues.rigor = detectRigor(
          String(nextValues.evidence),
          !!nextValues.use_cache,
          !!nextValues.escalate,
          !!nextValues.verify_writes,
        );
      }
      return { ...currentForm, values: nextValues };
    });
  }, []);

  const addTargetRoot = useCallback((value: string) => {
    const target = value.trim();
    if (!target) return;
    fieldRevisions.current.set('targets', (fieldRevisions.current.get('targets') ?? 0) + 1);
    setSaveError(null);
    setLoadedForm((currentForm) => {
      if (!currentForm) return currentForm;
      const targets = String(currentForm.values.targets ?? '')
        .split('\n')
        .map((entry) => entry.trim())
        .filter(Boolean);
      if (!targets.includes(target)) targets.push(target);
      return { ...currentForm, values: { ...currentForm.values, targets: targets.join('\n') } };
    });
  }, []);

  // The drag-drop handler lives at the app root and writes through this handle: reaching into a DOM
  // control directly would set a value React does not know about, and the next render would wipe it.
  useEffect(() => {
    apiRef.current = {
      setField: (key, value) => {
        if (key === 'targets') addTargetRoot(value);
        else setFieldValue(key, value);
      },
    };
    return () => { apiRef.current = null; };
  }, [addTargetRoot, apiRef, setFieldValue]);

  useEffect(() => {
    const requestId = loadRequestId.current + 1;
    loadRequestId.current = requestId;
    fieldRevisions.current.clear();
    pickerRequest.current = null;
    setLoadedForm(null);
    setDeleteConfirmationOpen(false);
    setDiscardConfirmationOpen(false);
    setSaveError(null);
    jumped.current = false;
    setMigratedFromSchema(null);
    setSchemaInspectionError(null);
    const load = async () => {
      let job: JobFull;
      let originalName: string | null = null;
      let jobId: string | null = null;
      let configRevision: string | null = null;
      try {
        if (name) {
          const detail = await getJob(name);
          job = detail.job;
          originalName = detail.name;
          jobId = detail.job_id;
          configRevision = detail.config_revision;
        } else {
          job = await defaultJob();
        }
      } catch (error) {
        if (!mounted.current || loadRequestId.current !== requestId) return;
        onStatusRef.current(`Failed to read job: ${error}`, 'err');
        onCloseRef.current();
        return;
      }
      if (!mounted.current || loadRequestId.current !== requestId) return;
      const initialValues = jobToForm(applyRigorPresetDefaults(job), originalName ?? '');
      setLoadedForm({
        base: job,
        values: initialValues,
        initialValues,
        originalName,
        jobId,
        configRevision,
      });
      if (name) {
        try {
          const schema = await jobFileSchema(name);
          if (mounted.current
            && loadRequestId.current === requestId
            && schema.on_disk < schema.current) setMigratedFromSchema(schema.on_disk);
        } catch (error) {
          if (mounted.current && loadRequestId.current === requestId) {
            setSchemaInspectionError(String(error));
          }
        }
      }
    };
    void load();
    return () => {
      if (loadRequestId.current === requestId) loadRequestId.current += 1;
    };
  }, [name]);

  // The pane mounts before its asynchronous fields, so navigation is reconciled once after loading.
  const jumped = useRef(false);
  useEffect(() => {
    if (jumped.current || !loadedForm || !formPane) return;
    const target = focusGroup && JOB_FORM_GROUPS.includes(focusGroup) ? focusGroup : JOB_FORM_GROUPS[0];
    if (!target) return;
    jumped.current = true;
    scrollTo(target, false);
  }, [loadedForm, formPane, focusGroup, scrollTo]);

  // The default-on presets arrive already written into `exclude` by `default_job()`. Seeding them here
  // as well would be a second implementation of the same policy, racing the IPC that supplies the list.
  const presetCatalog = useJunkPresetCatalog();

  const pickPathForField = async (key: string, kind: 'dir' | 'file') => {
    if (pickerRequest.current !== null) return;
    const requestId = nextPickerRequestId.current + 1;
    nextPickerRequestId.current = requestId;
    const request = {
      id: requestId,
      field: key,
      fieldRevision: fieldRevisions.current.get(key) ?? 0,
    };
    pickerRequest.current = request;
    const isDir = kind === 'dir';
    try {
      const currentValue = String(loadedForm?.values[key] ?? '').trim();
      const defaultPath = key === 'targets'
        ? currentValue.split('\n').map((entry) => entry.trim()).find(Boolean)
        : currentValue || undefined;
      const options = {
        title: isDir ? 'Select a directory' : 'Select an archive file',
        defaultPath,
      };
      const selectedPath = isDir ? await pickDirectory(options) : await pickSavePath(options);
      if (!mounted.current || pickerRequest.current !== request) return;
      if (selectedPath) {
        if ((fieldRevisions.current.get(key) ?? 0) !== request.fieldRevision) {
          onStatusRef.current('The picker result was ignored because that field changed while the picker was open');
          return;
        }
        if (key === 'targets') addTargetRoot(selectedPath);
        else setFieldValue(key, selectedPath);
      }
    } catch (error) {
      if (mounted.current && pickerRequest.current === request) {
        onStatusRef.current(`Can't open the picker: ${error}`, 'err');
      }
    } finally {
      if (pickerRequest.current === request) pickerRequest.current = null;
    }
  };

  const beginMutation = (kind: MutationKind): boolean => {
    if (mutationInFlight.current || busy) return false;
    mutationInFlight.current = true;
    setMutationKind(kind);
    return true;
  };

  const finishMutation = () => {
    mutationInFlight.current = false;
    if (mounted.current) setMutationKind(null);
  };

  const requestClose = () => {
    if (mutationInFlight.current) return;
    if (formDirty) {
      setDiscardConfirmationOpen(true);
      return;
    }
    onCloseRef.current();
  };

  const saveCurrentJob = async () => {
    if (!loadedForm || busy || mutationInFlight.current) return;
    const conversion = formToJob(loadedForm.values, loadedForm.base);
    if ('error' in conversion) {
      setSaveError({ message: conversion.error, field: conversion.field });
      onStatus(conversion.error, 'err');
      if (validationFocusFrame.current !== null) cancelAnimationFrame(validationFocusFrame.current);
      validationFocusFrame.current = requestAnimationFrame(() => {
        validationFocusFrame.current = null;
        if (!mounted.current) return;
        const fieldElement = formPane?.querySelector<HTMLElement>(`[data-field="${conversion.field}"]`);
        fieldElement?.focus({ preventScroll: true });
        fieldElement?.scrollIntoView({ block: 'center', inline: 'nearest' });
        // Restart the pulse even when Save is pressed twice without editing the field in between.
        // Toggling the class around a layout read is intentional: changing aria-invalid from true
        // to true would not restart a CSS animation on an already-invalid input.
        fieldElement?.classList.remove('field-error-pulse');
        if (fieldElement) void fieldElement.offsetWidth;
        fieldElement?.classList.add('field-error-pulse');
      });
      return;
    }
    if (!beginMutation('save')) return;
    setSaveError(null);
    try {
      if (loadedForm.originalName && (!loadedForm.jobId || !loadedForm.configRevision)) {
        throw new Error('The loaded job is missing registry identity or revision; close and reopen the editor');
      }
      const existing = loadedForm.originalName && loadedForm.jobId && loadedForm.configRevision
        ? { originalName: loadedForm.originalName, expectedRevision: loadedForm.configRevision }
        : undefined;
      const saved = await saveJob(conversion.name, conversion.job, existing);
      if (!mounted.current) return;
      const persistedJob = { ...conversion.job, job_id: saved.job_id };
      onSaved(
        saved,
        persistedJob,
        loadedForm.originalName && loadedForm.jobId && loadedForm.configRevision
          ? {
              jobId: loadedForm.jobId,
              name: loadedForm.originalName,
              configRevision: loadedForm.configRevision,
            }
          : null,
      );
    } catch (error) {
      if (!mounted.current) return;
      let message = `Save failed: ${error}`;
      try {
        await onMutationConflict(
          loadedForm.originalName ?? conversion.name,
          loadedForm.originalName && loadedForm.jobId && loadedForm.configRevision
            ? {
                jobId: loadedForm.jobId,
                name: loadedForm.originalName,
                configRevision: loadedForm.configRevision,
              }
            : null,
        );
        message += ' · refreshed the job registry; the editor kept your draft and did not overwrite any file';
      } catch (refreshError) {
        message += ` · job-registry refresh failed: ${refreshError}`;
      }
      if (!mounted.current) return;
      setSaveError({ message });
      onStatusRef.current(message, 'err');
    } finally {
      finishMutation();
    }
  };

  const deleteCurrentJob = async () => {
    if (!loadedForm?.originalName || !loadedForm.jobId || !loadedForm.configRevision || busy || mutationInFlight.current) return;
    if (!beginMutation('delete')) return;
    try {
      const deleted = await deleteJob(loadedForm.originalName, loadedForm.jobId, loadedForm.configRevision);
      if (!mounted.current) return;
      onDeleted(deleted);
    } catch (error) {
      if (!mounted.current) return;
      let message = `Delete failed: ${error}`;
      try {
        await onMutationConflict(loadedForm.originalName, {
          jobId: loadedForm.jobId,
          name: loadedForm.originalName,
          configRevision: loadedForm.configRevision,
        });
        message += ' · refreshed the job registry; no job file was deleted';
      } catch (refreshError) {
        message += ` · job-registry refresh failed: ${refreshError}`;
      }
      if (mounted.current) onStatusRef.current(message, 'err');
    } finally {
      finishMutation();
    }
  };

  const pathFieldClassName = (key: string) => dropTargetKey === key ? 'dropon' : '';

  return (
    <Sheet
      title={name ? `Edit job — ${name}` : 'New job'}
      width="xl"
      onClose={requestClose}
      footer={
        <>
          {name && <button type="button" className="btn danger" disabled={busy || mutationKind !== null} onClick={() => setDeleteConfirmationOpen(true)}>Delete job</button>}
          {name && (
            <button
              type="button"
              className="btn"
              title="Copy the schtasks command (run it yourself in an admin terminal; this app does not register system scheduled tasks for you)"
              disabled={busy || mutationKind !== null}
              onClick={() => {
                navigator.clipboard?.writeText(windowsScheduledTaskCommand(name)).then(
                  () => onStatus('Scheduled-task command copied — run it in an elevated terminal (this app does not register system tasks for you)', 'ok'),
                  () => onStatus('Copy failed (clipboard unavailable)', 'err'),
                );
              }}
            >Copy scheduled task command</button>
          )}
          {saveError && <span className="ed-save-error" role="alert">{saveError.message}</span>}
          <span className="spacer" />
          <button type="button" className="btn" disabled={mutationKind !== null} onClick={requestClose}>Cancel (Esc)</button>
          <button type="button" className="btn accent" disabled={!loadedForm || mutationKind !== null || busy} onClick={saveCurrentJob}>
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
        {/* Every section stays mounted, which keeps root drop fields in the DOM at every scroll
            position. Rendering only the visible section would make drag-and-drop depend on the
            active navigation item. */}
        <div className="ed-body">
          <nav className="ed-nav">
            {JOB_FORM_GROUPS.map((group) => (
              <button type="button" key={group} className={group === active ? 'on' : ''} onClick={() => scrollTo(group)}>{group}</button>
            ))}
          </nav>
          <div className="ed-pane" ref={setFormPaneRef}>
            {loadedForm && JOB_FORM_GROUPS.map((group) => (
              <section key={group} className="ed-section" ref={register(group)}>
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
              onConfirm: () => void deleteCurrentJob(),
            }]}
            onCancel={() => { if (!mutationInFlight.current) setDeleteConfirmationOpen(false); }}
          />
        )}
        {discardConfirmationOpen && (
          <ConfirmDialog
            title={name ? `Discard unsaved changes to '${name}'?` : 'Discard this new job draft?'}
            message="The editor contains changes that have not been saved. Closing it now will discard this draft."
            actions={[{
              label: 'Discard unsaved changes',
              danger: true,
              onConfirm: () => {
                setDiscardConfirmationOpen(false);
                onCloseRef.current();
              },
            }]}
            onCancel={() => setDiscardConfirmationOpen(false)}
          />
        )}
      </>
    </Sheet>
  );
}
