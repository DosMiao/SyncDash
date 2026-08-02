import { FolderOpen, Info } from 'lucide-react';
import { Menu } from '#ui/shared/components/menu/Menu.tsx';
import type { ReactNode } from 'react';
import type { FormFieldSpec, FormValues } from '#core/domain/jobs/formSchema.ts';

export interface SchemaSectionProps {
  fields: FormFieldSpec[];
  values: FormValues;
  set: (key: string, value: string | boolean) => void;
  onPick?: (key: string, kind: 'dir' | 'file') => void;
  pathClass?: (key: string) => string;
  /// Authorizes the application-level Tauri drop handler to fill path fields in this section.
  droppable?: boolean;
  after?: (key: string) => ReactNode;
  disabledField?: (key: string) => boolean;
  readOnlyField?: (key: string) => boolean;
  numericBounds?: (key: string) => { min: number; max: number; step: number } | undefined;
  autoFocusField?: string;
  invalidField?: string;
  /// A custom field renders caller-owned controls and has no value of its own.
  renderCustom?: (field: FormFieldSpec) => ReactNode;
}

function Help({ text }: { text: string }) {
  return (
    <Menu className="ed-help" title="What this does" trigger={<Info size={13} />}>
      <div className="ed-help-body">{text}</div>
    </Menu>
  );
}

function SchemaField({ field, section }: { field: FormFieldSpec; section: SchemaSectionProps }) {
  const {
    values, set, onPick, pathClass, droppable, disabledField, readOnlyField,
    numericBounds, renderCustom, autoFocusField, invalidField,
  } = section;
  const value = values[field.key];
  const disabled = disabledField?.(field.key) ?? false;
  const commonAttributes = {
    'data-field': field.key,
    'aria-invalid': invalidField === field.key || undefined,
    autoFocus: autoFocusField === field.key,
  };

  const fieldHeading = (
    <span className="ed-field-head">
      <span className="field-label">{field.label}</span>
      {field.help && <Help text={field.help} />}
    </span>
  );
  const description = field.desc ? <span className="hint">{field.desc}</span> : null;

  if (field.kind === 'bool') {
    return (
      <label className="ed-field ed-check">
        <input
          {...commonAttributes}
          type="checkbox"
          checked={Boolean(value)}
          disabled={disabled}
          onChange={(event) => set(field.key, event.target.checked)}
        />
        <span className="ed-field-body">{fieldHeading}{description}</span>
      </label>
    );
  }

  let control: ReactNode;
  if (field.kind === 'select') {
    control = (
      <select
        {...commonAttributes}
        value={String(value ?? '')}
        disabled={disabled}
        onChange={(event) => set(field.key, event.target.value)}
      >
        {field.opts!.map((option) => <option key={option} value={option}>{option}</option>)}
      </select>
    );
  } else if (field.kind === 'lines') {
    control = (
      <div className={field.key === 'targets' ? 'pathrow' : undefined}>
        <textarea
          {...commonAttributes}
          className={pathClass?.(field.key) ?? ''}
          data-drop={droppable && field.key === 'targets' ? '1' : undefined}
          data-field-key={field.key}
          spellCheck={false}
          value={String(value ?? '')}
          disabled={disabled}
          onChange={(event) => set(field.key, event.target.value)}
        />
        {field.key === 'targets' && onPick && (
          <button
            type="button"
            className="pbtn"
            title="Add a target directory…"
            onClick={() => onPick(field.key, 'dir')}
          ><FolderOpen size={13} /></button>
        )}
      </div>
    );
  } else if (field.kind === 'num') {
    const bounds = numericBounds?.(field.key);
    control = (
      <input
        {...commonAttributes}
        type="number"
        step={bounds?.step ?? 'any'}
        min={bounds?.min}
        max={bounds?.max}
        value={String(value ?? '')}
        disabled={disabled}
        onChange={(event) => set(field.key, event.target.value)}
      />
    );
  } else if (field.kind === 'dir' || field.kind === 'file') {
    const pathFieldKind: 'dir' | 'file' = field.kind === 'dir' ? 'dir' : 'file';
    control = (
      <div className="pathrow">
        <input
          {...commonAttributes}
          type="text"
          className={pathClass?.(field.key) ?? ''}
          data-drop={droppable ? '1' : undefined}
          data-field-key={field.key}
          list="sd-paths"
          spellCheck={false}
          value={String(value ?? '')}
          disabled={disabled}
          readOnly={readOnlyField?.(field.key) ?? false}
          onChange={(event) => set(field.key, event.target.value)}
        />
        {onPick && (
          <button
            type="button"
            className="pbtn"
            title="Browse…"
            disabled={disabled}
            onClick={() => onPick(field.key, pathFieldKind)}
          ><FolderOpen size={13} /></button>
        )}
      </div>
    );
  } else if (field.kind === 'custom') {
    control = renderCustom?.(field);
  } else {
    control = (
      <input
        {...commonAttributes}
        type="text"
        spellCheck={false}
        value={String(value ?? '')}
        disabled={disabled}
        onChange={(event) => set(field.key, event.target.value)}
      />
    );
  }

  // Custom fields own labelled controls, so an outer label would create invalid nested labels.
  const FieldContainer = field.kind === 'custom' ? 'div' : 'label';
  return (
    <FieldContainer className={`ed-field ed-kind-${field.kind}`}>
      {fieldHeading}
      {control}
      {description}
    </FieldContainer>
  );
}

export function SchemaSection({ fields, ...props }: SchemaSectionProps) {
  const sectionProps = { fields, ...props };
  return (
    <div className="box">
      {fields.filter((field) => !field.parent).map((field) => {
        const childFields = fields.filter((candidate) => candidate.parent === field.key);
        return (
          <div key={field.key} className="box-row">
            <SchemaField field={field} section={sectionProps} />
            {childFields.length > 0 && (
              <div className="ed-child">
                {childFields.map((childField) => (
                  <SchemaField key={childField.key} field={childField} section={sectionProps} />
                ))}
              </div>
            )}
            {props.after?.(field.key)}
          </div>
        );
      })}
    </div>
  );
}
