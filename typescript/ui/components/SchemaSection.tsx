import { ArrowLeftRight, FolderOpen, Info } from 'lucide-react';
import { Menu } from './ui';
import type { ReactNode } from 'react';
import type { FormValues, FSpec } from '../../core/jobfields';

// One renderer for both schema-driven sheets (job editor, log settings). The two differ only in
// which optional affordances they pass in — a browse button, the swap, the health-check verdict
// slot — so they stay one implementation and cannot drift apart visually.
//
// Every row is the same three-part shape: **name, control, one short line**. The three are given
// different size, weight and color by .field-label / .hint, which is the whole point — the rule
// this replaced gave a field's name and its explanation identical typography, so the form read as
// an undifferentiated wall. Anything longer than that one line goes behind the info icon.

export interface SchemaSectionProps {
  fields: FSpec[];
  values: FormValues;
  set: (key: string, value: string | boolean) => void;
  onPick?: (key: string, kind: 'dir' | 'file') => void;
  /// Rendered on the `source` row only; the job editor passes it, settings does not
  onSwap?: () => void;
  /// Extra class for a path input — 'good' | 'bad' | 'dropon' from the two-root health check
  pathClass?: (key: string) => string;
  /// Marks path inputs the Tauri drag-drop handler is allowed to fill (see App's dropScope)
  droppable?: boolean;
  /// Node injected after a given field, e.g. the two-root verdict box under `target`
  after?: (key: string) => ReactNode;
  disabledField?: (key: string) => boolean;
  /// The job editor uses these to make an invalid off-screen field discoverable. Settings leaves
  /// both unset, so the shared renderer keeps its ordinary behavior there.
  autoFocusField?: string;
  invalidField?: string;
  /// Body for a `custom` field — the junk-preset checkbox block, which edits `exclude` rather than
  /// holding a form value of its own
  renderCustom?: (f: FSpec) => ReactNode;
}

/// The paragraph, behind an icon. A Menu rather than a `title`: these run to several sentences and
/// a native tooltip both truncates them and vanishes while you are still reading.
function Help({ text }: { text: string }) {
  return (
    <Menu className="ed-help" title="What this does" trigger={<Info size={13} />}>
      <div className="ed-help-body">{text}</div>
    </Menu>
  );
}

function Field({ f, props }: { f: FSpec; props: SchemaSectionProps }) {
  const {
    values, set, onPick, onSwap, pathClass, droppable, disabledField, renderCustom,
    autoFocusField, invalidField,
  } = props;
  const v = values[f.key];
  const disabled = disabledField?.(f.key) ?? false;
  const common = {
    'data-field': f.key,
    'aria-invalid': invalidField === f.key || undefined,
    autoFocus: autoFocusField === f.key,
  };

  const name = (
    <span className="ed-field-head">
      <span className="field-label">{f.label}</span>
      {f.help && <Help text={f.help} />}
    </span>
  );
  const desc = f.desc ? <span className="hint">{f.desc}</span> : null;

  // A boolean leads with its control: the checkbox *is* the subject, and a label floating above a
  // tickbox reads as a heading for the whole group rather than that one row's name
  if (f.kind === 'bool') {
    return (
      <label className="ed-field ed-check">
        <input {...common} type="checkbox" checked={!!v} disabled={disabled} onChange={(e) => set(f.key, e.target.checked)} />
        <span className="ed-field-body">{name}{desc}</span>
      </label>
    );
  }

  let control: ReactNode;
  if (f.kind === 'select') {
    control = (
      <select {...common} value={String(v ?? '')} disabled={disabled} onChange={(e) => set(f.key, e.target.value)}>
        {f.opts!.map((o) => <option key={o} value={o}>{o}</option>)}
      </select>
    );
  } else if (f.kind === 'lines') {
    control = (
      <textarea {...common} spellCheck={false} value={String(v ?? '')} disabled={disabled} onChange={(e) => set(f.key, e.target.value)} />
    );
  } else if (f.kind === 'num') {
    control = (
      <input {...common} type="number" step="any" value={String(v ?? '')} disabled={disabled} onChange={(e) => set(f.key, e.target.value)} />
    );
  } else if (f.kind === 'dir' || f.kind === 'file') {
    control = (
      <div className="pathrow">
        <input
          {...common}
          type="text"
          className={pathClass?.(f.key) ?? ''}
          data-drop={droppable ? '1' : undefined}
          data-field-key={f.key}
          list="sd-paths"
          spellCheck={false}
          value={String(v ?? '')}
          disabled={disabled}
          onChange={(e) => set(f.key, e.target.value)}
        />
        {f.key === 'source' && onSwap && (
          <button type="button" className="pbtn" title="Swap with target" onClick={onSwap}>
            <ArrowLeftRight size={13} />
          </button>
        )}
        {onPick && (
          <button
            type="button"
            className="pbtn"
            title="Browse…"
            onClick={() => onPick(f.key, f.kind as 'dir' | 'file')}
          ><FolderOpen size={13} /></button>
        )}
      </div>
    );
  } else if (f.kind === 'custom') {
    control = renderCustom?.(f);
  } else {
    control = (
      <input {...common} type="text" spellCheck={false} value={String(v ?? '')} disabled={disabled} onChange={(e) => set(f.key, e.target.value)} />
    );
  }

  // A label element wrapping the control, except for `custom`: that slot holds a whole grid of its
  // own labelled checkboxes, and nesting labels is invalid HTML — the browser may forward a click
  // to the outer label's control, i.e. the *first* checkbox in the grid, so ticking "Developer"
  // would silently tick "Windows" instead.
  const Wrap = f.kind === 'custom' ? 'div' : 'label';
  return (
    <Wrap className={`ed-field ed-kind-${f.kind}`}>
      {name}
      {control}
      {desc}
    </Wrap>
  );
}

/**
 * One section as a single bordered box, one row per setting.
 *
 * One box per *section*, not per field: thirty boxes is thirty borders and no grouping, which is
 * the same undifferentiated wall in a different costume. This is how the reference groups its own
 * settings — a titled panel, rows inside it, hairlines between.
 */
export function SchemaSection({ fields, ...props }: SchemaSectionProps) {
  const all = { fields, ...props };
  return (
    <div className="box">
      {fields.filter((f) => !f.parent).map((f) => {
        const kids = fields.filter((k) => k.parent === f.key);
        return (
          <div key={f.key} className="box-row">
            <Field f={f} props={all} />
            {kids.length > 0 && (
              <div className="ed-child">
                {kids.map((k) => <Field key={k.key} f={k} props={all} />)}
              </div>
            )}
            {props.after?.(f.key)}
          </div>
        );
      })}
    </div>
  );
}
