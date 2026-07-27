import { useEffect, useState } from 'react';
import { SET_FIELDS, formToSettings, settingsToForm } from '../../core/jobfields';
import { getSettings, pickPath, saveSettings } from '../../core/ipc';
import { SchemaForm } from './SchemaForm';
import type { FormValues } from '../../core/jobfields';

interface Props {
  onClose: () => void;
  onSaved: (msg: string, cls: '' | 'err' | 'ok') => void;
  onStatus: (msg: string, cls?: '' | 'err' | 'ok') => void;
}

export function SettingsSheet({ onClose, onSaved, onStatus }: Props) {
  const [values, setValues] = useState<FormValues | null>(null);

  useEffect(() => {
    let live = true;
    getSettings()
      .then((s) => { if (live) setValues(settingsToForm(s)); })
      .catch((e) => { onStatus(`Failed to read settings: ${e}`, 'err'); onClose(); });
    return () => { live = false; };
  }, []);

  const save = async () => {
    if (!values) return;
    try {
      const rep = await saveSettings(formToSettings(values));
      const moved = rep.moved ? `, migrated ${rep.moved} items` : '';
      const failed = rep.failed ? `, ${rep.failed} failed` : '';
      onSaved(`Log settings saved${moved}${failed}`, rep.failed ? 'err' : 'ok');
    } catch (e) {
      onStatus(`Failed to save settings: ${e}`, 'err');
    }
  };

  return (
    <div id="setmodal" onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div className="sheet setsheet">
        <h3>Log settings</h3>
        <div id="set-form">
          {values && (
            <SchemaForm
              fields={SET_FIELDS}
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
          )}
        </div>
        <div id="set-note" className="thin">
          Change the log directory and save, and the old directory moves wholesale to the new location
          (across volumes it falls back to copy + delete).
        </div>
        <div className="btnrow">
          <button className="btn" onClick={onClose}>Cancel (Esc)</button>
          <button className="btn accent" onClick={save}>Save</button>
        </div>
      </div>
    </div>
  );
}
