import { useCallback, useEffect, useRef, useState } from 'react';
import { TriangleAlert } from 'lucide-react';
import {
  SETTINGS_FORM_FIELDS,
  formFieldsInGroup,
  formToSettings,
  formGroupNames,
  settingsToForm,
} from '#core/domain/jobs/formSchema.ts';
import { getSettings, pickLogDirectory, saveSettings } from '#core/infrastructure/tauri/commands/settings.ts';
import { SchemaSection } from '#ui/features/jobs/components/SchemaSection.tsx';
import { Sheet } from '#ui/shared/components/overlays/Sheet.tsx';
import type { FormValues } from '#core/domain/jobs/formSchema.ts';
import type { SettingsSnapshotDto } from '#core/types/generated/SettingsSnapshotDto.ts';
import type { StatusAppearance } from '#ui/shared/status/useStatus.ts';

const SETTINGS_FORM_GROUPS = formGroupNames(SETTINGS_FORM_FIELDS);

export interface SettingsSheetProps {
  onClose: () => void;
  onSaved: (message: string, appearance: StatusAppearance) => void;
  onStatus: (message: string, appearance?: StatusAppearance) => void;
}

type SettingsMutationKind = 'picking-log-directory' | 'saving-settings';

export function SettingsSheetController({ onClose, onSaved, onStatus }: SettingsSheetProps) {
  const [settingsSnapshot, setSettingsSnapshot] = useState<SettingsSnapshotDto | null>(null);
  const [formValues, setFormValues] = useState<FormValues | null>(null);
  const [logDirectoryGrantId, setLogDirectoryGrantId] = useState<string | null>(null);
  const [activeMutationKind, setActiveMutationKind] = useState<SettingsMutationKind | null>(null);
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [errorMessage, setErrorMessage] = useState('');
  const [invalidFieldName, setInvalidFieldName] = useState('');
  const componentMounted = useRef(false);
  const loadRequestSequence = useRef(0);
  const activeMutation = useRef<{ id: symbol; kind: SettingsMutationKind } | null>(null);

  useEffect(() => {
    componentMounted.current = true;
    return () => {
      componentMounted.current = false;
      loadRequestSequence.current += 1;
    };
  }, []);

  const loadSettings = useCallback(async () => {
    const requestSequence = loadRequestSequence.current + 1;
    loadRequestSequence.current = requestSequence;
    setSettingsLoading(true);
    setErrorMessage('');
    try {
      const loaded = await getSettings();
      if (!componentMounted.current || loadRequestSequence.current !== requestSequence) return;
      setSettingsSnapshot(loaded);
      setFormValues(settingsToForm(loaded.settings));
      setLogDirectoryGrantId(null);
    } catch (loadError) {
      if (!componentMounted.current || loadRequestSequence.current !== requestSequence) return;
      const message = `Failed to read settings: ${loadError}`;
      setErrorMessage(message);
      onStatus(message, 'err');
    } finally {
      if (componentMounted.current && loadRequestSequence.current === requestSequence) {
        setSettingsLoading(false);
      }
    }
  }, [onStatus]);

  useEffect(() => { void loadSettings(); }, [loadSettings]);

  const beginMutation = (kind: SettingsMutationKind): symbol | null => {
    if (activeMutation.current) return null;
    const id = Symbol(kind);
    activeMutation.current = { id, kind };
    setActiveMutationKind(kind);
    return id;
  };

  const ownsMutation = (id: symbol): boolean => activeMutation.current?.id === id;

  const finishMutation = (id: symbol) => {
    if (!ownsMutation(id)) return;
    activeMutation.current = null;
    if (componentMounted.current) setActiveMutationKind(null);
  };

  const closeIfIdle = () => {
    if (!activeMutation.current) onClose();
  };

  const chooseLogDirectory = async () => {
    if (!settingsSnapshot || !formValues) return;
    const requestId = beginMutation('picking-log-directory');
    if (!requestId) return;
    setErrorMessage('');
    try {
      const selection = await pickLogDirectory(settingsSnapshot.revision);
      if (!componentMounted.current || !ownsMutation(requestId) || !selection) return;
      setFormValues((current) => (current ? { ...current, log_dir: selection.directory } : current));
      setLogDirectoryGrantId(selection.grant_id);
    } catch (pickerError) {
      if (!componentMounted.current || !ownsMutation(requestId)) return;
      const message = `Failed to choose the log directory: ${pickerError}`;
      setErrorMessage(message);
      onStatus(message, 'err');
    } finally {
      finishMutation(requestId);
    }
  };

  const saveCurrentSettings = async () => {
    if (!settingsSnapshot || !formValues) return;
    const conversion = formToSettings(formValues, settingsSnapshot.numeric_limits);
    if ('error' in conversion) {
      setErrorMessage(conversion.error);
      setInvalidFieldName(conversion.field);
      return;
    }
    const requestId = beginMutation('saving-settings');
    if (!requestId) return;
    setErrorMessage('');
    setInvalidFieldName('');
    try {
      const saved = await saveSettings(
        conversion.settings,
        settingsSnapshot.revision,
        logDirectoryGrantId ?? undefined,
      );
      if (!componentMounted.current || !ownsMutation(requestId)) return;
      setSettingsSnapshot(saved.snapshot);
      setFormValues(settingsToForm(saved.snapshot.settings));
      setLogDirectoryGrantId(null);
      const report = saved.migration;
      const moved = report.moved ? `, migrated ${report.moved} items` : '';
      const skipped = report.skipped ? `, kept ${report.skipped} colliding items in the old location` : '';
      const failed = report.failed ? `, ${report.failed} failed` : '';
      const detail = report.messages.length ? ` — ${report.messages.join('; ')}` : '';
      finishMutation(requestId);
      onSaved(
        `Log settings saved and active${moved}${skipped}${failed}${detail}`,
        report.failed ? 'err' : 'ok',
      );
    } catch (saveError) {
      if (!componentMounted.current || !ownsMutation(requestId)) return;
      const message = `Failed to save settings: ${saveError}`;
      setErrorMessage(message);
      onStatus(message, 'err');
    } finally {
      finishMutation(requestId);
    }
  };

  const mutationPending = activeMutationKind !== null;
  return (
    <Sheet
      title="Log settings"
      width="mid"
      onClose={closeIfIdle}
      footer={
        <>
          <span className="spacer" />
          {!settingsSnapshot && (
            <button type="button" className="btn" disabled={settingsLoading || mutationPending} onClick={() => void loadSettings()}>
              {settingsLoading ? 'Loading…' : 'Retry'}
            </button>
          )}
          <button type="button" className="btn" disabled={mutationPending} onClick={closeIfIdle}>Cancel (Esc)</button>
          <button type="button" className="btn accent" disabled={!formValues || !settingsSnapshot || mutationPending} onClick={() => void saveCurrentSettings()}>
            {activeMutationKind === 'saving-settings' ? 'Saving…' : 'Save'}
          </button>
        </>
      }
    >
      <div className="ed-pane">
        {settingsSnapshot?.diagnostic && (
          <div className="ed-notice" role="alert">
            <span className="edn-mark"><TriangleAlert size={14} /></span>
            <span>{settingsSnapshot.diagnostic}</span>
          </div>
        )}
        {errorMessage && (
          <div className="ed-notice settings-error" role="alert">
            <span className="edn-mark"><TriangleAlert size={14} /></span>
            <span>{errorMessage}</span>
          </div>
        )}
        {settingsLoading && !formValues && <div className="set-note hint" role="status">Loading settings…</div>}
        {formValues && settingsSnapshot && SETTINGS_FORM_GROUPS.map((group) => (
          <div key={group} className="ed-section">
            <div className="section-title">{group}</div>
            <SchemaSection
              fields={formFieldsInGroup(SETTINGS_FORM_FIELDS, group)}
              values={formValues}
              set={(key, value) => {
                if (activeMutation.current) return;
                setInvalidFieldName((current) => (current === key ? '' : current));
                setFormValues((current) => (current ? { ...current, [key]: value } : current));
              }}
              onPick={(key) => { if (key === 'log_dir') void chooseLogDirectory(); }}
              disabledField={() => mutationPending}
              readOnlyField={(key) => key === 'log_dir'}
              numericBounds={(key) => {
                if (key === 'keep_days') {
                  return { min: 0, max: settingsSnapshot.numeric_limits.maximum_keep_days, step: 1 };
                }
                if (key === 'max_total_mb') {
                  return { min: 0, max: settingsSnapshot.numeric_limits.maximum_total_mb, step: 1 };
                }
                return undefined;
              }}
              invalidField={invalidFieldName}
              after={(key) => key === 'log_dir' && formValues.log_dir !== '' ? (
                <button
                  type="button"
                  className="btn"
                  disabled={mutationPending}
                  onClick={() => {
                    if (activeMutation.current) return;
                    setFormValues((current) => (current ? { ...current, log_dir: '' } : current));
                    setLogDirectoryGrantId(null);
                  }}
                >Use Default Location</button>
              ) : null}
            />
          </div>
        ))}
        {formValues && (
          <div className="set-note hint">
            A changed location is accepted only from Browse. Saving migrates existing history;
            colliding run folders stay in the old location rather than being overwritten.
          </div>
        )}
      </div>
    </Sheet>
  );
}
