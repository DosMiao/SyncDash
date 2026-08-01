import { useCallback, useEffect, useRef, useState } from 'react';
import { CornerRightUp } from 'lucide-react';
import {
  ED_FIELDS, RIGOR_PRESETS, applyRigorPresetDefaults, detectRigor, fieldsInGroup, formToJob,
  groupsOf, jobToForm, schtasksCmd,
} from '../../core/jobfields';
import { defaultJob, deleteJob, getJob, jobFileSchema, pickPath, saveJob } from '../../core/ipc';
import { pathState, usePathVerdict } from '../hooks/usePathVerdict';
import { JunkPresets, useJunkPresets } from './JunkPresets';
import { PathVerdictBox } from './PathVerdictBox';
import { SchemaSection } from './SchemaSection';
import { ConfirmDialog, Sheet } from './ui';
import { useScrollSpy } from '../hooks/useScrollSpy';
import type { FormValues } from '../../core/jobfields';
import type { JobFull } from '../../core/ipc';

export interface EditorApi { setField: (key: string, value: string) => void }

const ED_GROUPS = groupsOf(ED_FIELDS);

interface Props {
  /// null = a new job
  name: string | null;
  /// Section to open on. The config pills on the main screen each name the section that owns the
  /// setting they show, so clicking one lands you on it rather than scrolling you near it.
  focusGroup?: string;
  /// Field key the Tauri drag handler is hovering, for the drop highlight
  dropOn: string | null;
  /// Registers the form as the drop region. While the editor is open it takes precedence over the
  /// main screen's two roots, and unmounting clears it — which is the whole reason it is a callback ref.
  scopeRef: (el: HTMLElement | null) => void;
  apiRef: React.RefObject<EditorApi | null>;
  onClose: () => void;
  onSaved: (name: string, job: JobFull) => void;
  onDeleted: (name: string) => void;
  onStatus: (msg: string, cls?: '' | 'err' | 'ok') => void;
}

/// The form's whole state, loaded as one unit. `base` is the job the editor was opened on, kept intact:
/// every field ED_FIELDS does not surface rides back out from here on save. It is one state and not two
/// so the two halves cannot drift apart or arrive separately — a save that found values but no base
/// would have to bail out silently, and a silent no-op on the Save button is the worst kind of dead end.
interface Loaded { base: JobFull; values: FormValues }

interface SaveError { message: string; field?: string }

export function JobEditor(props: Props) {
  const { name, focusGroup, dropOn, scopeRef, apiRef, onClose, onSaved, onDeleted, onStatus } = props;
  const [form, setForm] = useState<Loaded | null>(null);
  /// Set when the file on disk predates the current job schema, i.e. the exclude lines on screen were
  /// produced by the load-time migration and are not in the file yet
  const [migratedFrom, setMigratedFrom] = useState<number | null>(null);
  const [askDelete, setAskDelete] = useState(false);
  const [saveError, setSaveError] = useState<SaveError | null>(null);
  const [saving, setSaving] = useState(false);
  /// The pane is the scroll container and the drop region both; it goes through state rather than a
  /// ref so the scrollspy re-subscribes when the node actually arrives
  const [pane, setPane] = useState<HTMLDivElement | null>(null);
  const { active, register, scrollTo } = useScrollSpy(pane, ED_GROUPS);

  // The pane is two things at once: the scrollspy's container and the drag-drop region. One stable
  // callback, not an inline arrow — React detaches and reattaches a ref whose identity changed, so
  // an inline one would null out both on every render.
  const paneRef = useCallback((el: HTMLDivElement | null) => {
    setPane(el);
    scopeRef(el);
  }, [scopeRef]);

  const set = useCallback((key: string, value: string | boolean) => {
    setSaveError(null);
    setForm((f) => {
      if (!f) return f;
      const next = { ...f.values, [key]: value };
      // Preset = macro: pick a preset → the four detail fields fill in; change any one by hand → the
      // preset jumps to custom (or back to a preset's name if the combination happens to match one)
      if (key === 'rigor') {
        const p = RIGOR_PRESETS[String(value)];
        if (p) {
          next.evidence = p.evidence;
          next.use_cache = p.use_cache;
          next.escalate = p.escalate;
          next.verify_writes = p.verify_writes;
        }
      } else if (key === 'evidence' || key === 'use_cache' || key === 'escalate' || key === 'verify_writes') {
        next.rigor = detectRigor(
          String(next.evidence), !!next.use_cache, !!next.escalate, !!next.verify_writes,
        );
      }
      return { ...f, values: next };
    });
  }, []);

  // The drag-drop handler lives at the app root and writes through this handle: reaching into the DOM
  // input directly would set a value React does not know about, and the next render would wipe it
  useEffect(() => {
    apiRef.current = { setField: (k, v) => set(k, v) };
    return () => { apiRef.current = null; };
  }, [apiRef, set]);

  useEffect(() => {
    let live = true;
    const load = async () => {
      let j: JobFull;
      try {
        // A new job's starting point is engine policy, so it comes from the engine
        j = name ? await getJob(name) : await defaultJob();
      } catch (e) {
        onStatus(`Failed to read job: ${e}`, 'err');
        onClose();
        return;
      }
      if (!live) return;
      // The rigor detail knobs may be null in an older file (meaning "follow the preset"); the form
      // materializes all four and writes them explicitly on save
      setForm({ base: j, values: jobToForm(applyRigorPresetDefaults(j), name ?? '') });
      if (name) {
        jobFileSchema(name)
          .then((s) => { if (live && s.on_disk < s.current) setMigratedFrom(s.on_disk); })
          .catch(() => { /* not knowing is not worth blocking the editor over */ });
      }
    };
    void load();
    return () => { live = false; };
  }, [name]);

  // Open on the section a config pill named, or Basics for an ordinary open. The pane mounts once
  // while the async form is still empty; without this post-load jump the scrollspy can keep its
  // temporary "last section" result even though the populated pane is at the top. Instant rather
  // than smooth: a dialog that appears already mid-glide reads as a glitch. Once only — a later
  const jumped = useRef(false);
  useEffect(() => {
    if (jumped.current || !form || !pane) return;
    const target = focusGroup && ED_GROUPS.includes(focusGroup) ? focusGroup : ED_GROUPS[0];
    if (!target) return;
    jumped.current = true;
    scrollTo(target, false);
  }, [form, pane, focusGroup, scrollTo]);

  // The default-on presets arrive already written into `exclude` by `default_job()`. Seeding them here
  // as well would be a second implementation of the same policy, racing the IPC that supplies the list.
  const presets = useJunkPresets();

  const source = String(form?.values.source ?? '');
  const target = String(form?.values.target ?? '');
  const verdict = usePathVerdict(source, target, !!form);

  const onPick = async (key: string, kind: 'dir' | 'file') => {
    const isDir = kind === 'dir';
    try {
      const p = await pickPath({
        directory: isDir,
        save: !isDir,
        title: isDir ? 'Select a directory' : 'Select an archive file',
        defaultPath: String(form?.values[key] ?? '').trim(),
      });
      if (p) set(key, p);
    } catch (e) {
      onStatus(`Can't open the picker: ${e}`, 'err');
    }
  };

  const save = async () => {
    if (!form) return;
    const c = formToJob(form.values, form.base);
    if ('error' in c) {
      setSaveError({ message: c.error, field: c.field });
      onStatus(c.error, 'err');
      requestAnimationFrame(() => {
        const el = pane?.querySelector<HTMLElement>(`[data-field="${c.field}"]`);
        el?.focus({ preventScroll: true });
        el?.scrollIntoView({ block: 'center', inline: 'nearest' });
        // Restart the pulse even when Save is pressed twice without editing the field in between.
        // Toggling the class around a layout read is intentional: changing aria-invalid from true
        // to true would not restart a CSS animation on an already-invalid input.
        el?.classList.remove('field-error-pulse');
        if (el) void el.offsetWidth;
        el?.classList.add('field-error-pulse');
      });
      return;
    }
    setSaveError(null);
    setSaving(true);
    try {
      await saveJob(c.name, c.job);
      onSaved(c.name, c.job);
    } catch (e) {
      const message = `Save failed: ${e}`;
      setSaveError({ message });
      onStatus(message, 'err');
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    if (!name) return;
    try {
      await deleteJob(name);
      onDeleted(name);
    } catch (e) {
      onStatus(`Delete failed: ${e}`, 'err');
    }
  };

  const pathClass = (key: string) => {
    const base = key === 'source' ? pathState(verdict?.source, source)
      : key === 'target' ? pathState(verdict?.target, target)
      : '';
    return [base, dropOn === key ? 'dropon' : ''].filter(Boolean).join(' ');
  };

  return (
    <Sheet
      title={name ? `Edit job — ${name}` : 'New job'}
      width="xl"
      onClose={onClose}
      footer={
        <>
          {name && <button className="btn danger" onClick={() => setAskDelete(true)}>Delete job</button>}
          {name && (
            <button
              className="btn"
              title="Copy the schtasks command (run it yourself in an admin terminal; this app does not register system scheduled tasks for you)"
              onClick={() => {
                navigator.clipboard?.writeText(schtasksCmd(name)).then(
                  () => onStatus('Scheduled-task command copied — run it in an elevated terminal (this app does not register system tasks for you)', 'ok'),
                  () => onStatus('Copy failed (clipboard unavailable)', 'err'),
                );
              }}
            >Copy scheduled task command</button>
          )}
          {saveError && <span className="ed-save-error" role="alert">{saveError.message}</span>}
          <span className="spacer" />
          <button className="btn" onClick={onClose}>Cancel (Esc)</button>
          <button className="btn accent" disabled={!form || saving} onClick={save}>
            {saving ? 'Saving…' : 'Save'}
          </button>
        </>
      }
    >
      <>
        {migratedFrom !== null && (
          <div className="ed-notice">
            <span className="edn-mark"><CornerRightUp size={14} /></span>
            <span>
              This job file is at schema v{migratedFrom}: its junk exclusions were kept in
              {' '}<code>os_excludes</code> / <code>dev_excludes</code>, and loading it wrote the matching
              patterns into the exclude list under <b>Filters</b>. The filter is unchanged — those lines
              are what the old keys already meant. They are not in the file yet; saving writes them out.
            </span>
          </div>
        )}
        {/* Every section stays mounted, which is also what keeps the two root inputs in the DOM at
            any scroll position — rendering only the visible one would silently break drag-and-drop
            onto source/target from anywhere but Basics. */}
        <div className="ed-body">
          <nav className="ed-nav">
            {ED_GROUPS.map((g) => (
              <button key={g} className={g === active ? 'on' : ''} onClick={() => scrollTo(g)}>{g}</button>
            ))}
          </nav>
          <div className="ed-pane" ref={paneRef}>
            {form && ED_GROUPS.map((g) => (
              <section key={g} className="ed-section" ref={register(g)}>
                <h4 className="section-title ed-section-title">{g}</h4>
                <SchemaSection
                  fields={fieldsInGroup(ED_FIELDS, g)}
                  values={form.values}
                  set={set}
                  onPick={onPick}
                  onSwap={() => setForm((f) => (f ? { ...f, values: { ...f.values, source: f.values.target, target: f.values.source } } : f))}
                  pathClass={pathClass}
                  droppable
                  autoFocusField={name ? undefined : '__name'}
                  invalidField={saveError?.field}
                  // "Escalate on divergence" only means anything when the evidence is a sampled digest
                  disabledField={(k) => k === 'escalate' && form.values.evidence !== 'sampled'}
                  renderCustom={() => (
                    <JunkPresets
                      presets={presets}
                      excludeText={String(form.values.exclude ?? '')}
                      onChange={(t) => set('exclude', t)}
                    />
                  )}
                  // Warnings sit right under the two roots they talk about
                  after={(k) => (k === 'target'
                    ? <PathVerdictBox key="v" verdict={verdict} className="ed-verdict" />
                    : null)}
                />
              </section>
            ))}
          </div>
        </div>

        {/* Nested inside this sheet on purpose: the editor stays on screen behind it, so cancelling
            puts you back exactly where you were. Escape unwinds one layer at a time. */}
        {askDelete && name && (
          <ConfirmDialog
            title={`Delete the job config '${name}'?`}
            message={
              `This removes the job file only:\n\n· ${name}.toml is deleted from the jobs directory\n` +
              '· Neither root is touched — no file on either side is read, moved or removed\n' +
              '· Past run logs and anything already in the trash are left alone'
            }
            actions={[{ label: 'Delete the job file', danger: true, onConfirm: () => void remove() }]}
            onCancel={() => setAskDelete(false)}
          />
        )}
      </>
    </Sheet>
  );
}
