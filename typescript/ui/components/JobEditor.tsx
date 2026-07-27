import { useCallback, useEffect, useRef, useState } from 'react';
import {
  ED_FIELDS, RIGOR_PRESETS, applyRigorPresetDefaults, detectRigor, formToJob, jobToForm, schtasksCmd,
} from '../../core/jobfields';
import { defaultJob, deleteJob, getJob, jobFileSchema, pickPath, saveJob } from '../../core/ipc';
import { pathState, usePathVerdict } from '../hooks/usePathVerdict';
import { JunkPresets, useJunkPresets } from './JunkPresets';
import { PathVerdictBox } from './PathVerdictBox';
import { SchemaForm } from './SchemaForm';
import type { FormValues } from '../../core/jobfields';
import type { JobFull } from '../../core/ipc';

export interface EditorApi { setField: (key: string, value: string) => void }

interface Props {
  /// null = a new job
  name: string | null;
  focusGroup?: string;
  /// Field key the Tauri drag handler is hovering, for the drop highlight
  dropOn: string | null;
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

export function JobEditor(props: Props) {
  const { name, focusGroup, dropOn, apiRef, onClose, onSaved, onDeleted, onStatus } = props;
  const [form, setForm] = useState<Loaded | null>(null);
  /// Set when the file on disk predates the current job schema, i.e. the exclude lines on screen were
  /// produced by the load-time migration and are not in the file yet
  const [migratedFrom, setMigratedFrom] = useState<number | null>(null);
  const groups = useRef(new Map<string, HTMLElement>());

  const set = useCallback((key: string, value: string | boolean) => {
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

  useEffect(() => {
    if (!form || !focusGroup) return;
    groups.current.get(focusGroup)?.scrollIntoView({ block: 'start' });
  }, [form, focusGroup]);

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
    if ('error' in c) { onStatus(c.error, 'err'); return; }
    try {
      await saveJob(c.name, c.job);
      onSaved(c.name, c.job);
    } catch (e) {
      onStatus(`Save failed: ${e}`, 'err');
    }
  };

  const remove = async () => {
    if (!name) return;
    if (!window.confirm(`Delete the job config '${name}'? (Removes the TOML only — no data is touched.)`)) return;
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
    <div id="editmodal" onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div className="sheet edsheet">
        <h3>{name ? `Edit job — ${name}` : 'New job'}</h3>
        {migratedFrom !== null && (
          <div className="ed-notice">
            <span className="edn-mark">⤴</span>
            <span>
              This job file is at schema v{migratedFrom}: its junk exclusions were kept in
              {' '}<code>os_excludes</code> / <code>dev_excludes</code>, and loading it wrote the matching
              patterns into the exclude list below. The filter is unchanged — those lines are what the old
              keys already meant. They are not in the file yet; saving writes them out.
            </span>
          </div>
        )}
        <div id="ed-form">
          {form && (
            <SchemaForm
              fields={ED_FIELDS}
              values={form.values}
              set={set}
              onPick={onPick}
              onSwap={() => setForm((f) => (f ? { ...f, values: { ...f.values, source: f.values.target, target: f.values.source } } : f))}
              pathClass={pathClass}
              droppable
              // "Escalate on divergence" only means anything when the evidence is a sampled digest
              disabledField={(k) => k === 'escalate' && form.values.evidence !== 'sampled'}
              groupRef={(g, el) => { if (el) groups.current.set(g, el); }}
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
          )}
        </div>
        <div className="btnrow">
          {name && <button className="btn danger" onClick={remove}>Delete job</button>}
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
          <span className="spacer" />
          <button className="btn" onClick={onClose}>Cancel (Esc)</button>
          <button className="btn accent" onClick={save}>Save</button>
        </div>
      </div>
    </div>
  );
}
