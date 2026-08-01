import { useEffect, useState } from 'react';
import { SET_FIELDS, fieldsInGroup, formToSettings, groupsOf, settingsToForm } from '../../core/jobfields';
import { getSettings, pickPath, saveSettings } from '../../core/ipc';
import { SchemaSection } from './SchemaSection';
import { Sheet } from './ui';
import type { FormValues } from '../../core/jobfields';

/// Three short sections — they stack, where the job editor's six earn a rail
const SET_GROUPS = groupsOf(SET_FIELDS);

interface Props {
  onClose: () => void;
  onSaved: (msg: string, cls: '' | 'err' | 'ok') => void;
  onStatus: (msg: string, cls?: '' | 'err' | 'ok') => void;
}

export function SettingsSheet({ onClose, onSaved, onStatus }: Props) {
  const [values, setValues] = useState<FormValues | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');

  useEffect(() => {
    let live = true;
    getSettings()
      .then((s) => { if (live) setValues(settingsToForm(s)); })
      .catch((e) => { onStatus(`Failed to read settings: ${e}`, 'err'); onClose(); });
    return () => { live = false; };
  }, []);

  const save = async () => {
    if (!values || saving) return;
    setSaving(true);
    setError('');
    try {
      const rep = await saveSettings(formToSettings(values));
      const moved = rep.moved ? `, migrated ${rep.moved} items` : '';
      const skipped = rep.skipped ? `, kept ${rep.skipped} colliding items in the old location` : '';
      const failed = rep.failed ? `, ${rep.failed} failed` : '';
      const detail = rep.messages.length ? ` — ${rep.messages.join('; ')}` : '';
      onSaved(`Log settings saved and active${moved}${skipped}${failed}${detail}`, rep.failed ? 'err' : 'ok');
    } catch (e) {
      const message = `Failed to save settings: ${e}`;
      setError(message);
      onStatus(message, 'err');
    } finally {
      setSaving(false);
    }
  };

  return (
    <Sheet
      title="Log settings"
      width="mid"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>Cancel (Esc)</button>
          <button type="button" className="btn accent" disabled={!values || saving} onClick={save}>
            {saving ? 'Saving…' : 'Save'}
          </button>
        </>
      }
    >
      <div className="ed-pane">
        {error && <div className="alert err" role="alert">{error}</div>}
        {values && SET_GROUPS.map((g) => (
          <div key={g} className="ed-section">
            <div className="section-title">{g}</div>
            <SchemaSection
              fields={fieldsInGroup(SET_FIELDS, g)}
              values={values}
              set={(k, v) => setValues((prev) => (prev ? { ...prev, [k]: v } : prev))}
              onPick={async (key) => {
                try {
                  const p = await pickPath({
                    directory: true, title: 'Select a log directory', defaultPath: String(values[key] ?? '').trim(),
                  });
                  if (p) setValues((prev) => (prev ? { ...prev, [key]: p } : prev));
                } catch (e) {
                  onStatus(`Can't open the picker: ${e}`, 'err');
                }
              }}
            />
          </div>
        ))}
        <div className="set-note hint">
          Change the log directory and save, and the old directory moves wholesale to the new location
          (across volumes it falls back to copy + delete).
        </div>
      </div>
    </Sheet>
  );
}
