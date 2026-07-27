import { Fragment } from 'react';
import type { ReactNode } from 'react';
import type { FormValues, FSpec } from '../../core/jobfields';

// One renderer for both schema-driven sheets (job editor, log settings). The two differ only in which
// optional affordances they pass in — a browse button, the ⇄ swap, the health-check verdict slot — so
// they stay one implementation and cannot drift apart visually.

export interface SchemaFormProps {
  fields: FSpec[];
  values: FormValues;
  set: (key: string, value: string | boolean) => void;
  onPick?: (key: string, kind: 'dir' | 'file') => void;
  /// Rendered on the `source` row only; the job editor passes it, settings does not
  onSwap?: () => void;
  /// Extra class for a path input — 'good' | 'bad' | 'dropon' from the two-root health check
  pathClass?: (key: string) => string;
  /// Marks path inputs the Tauri drag-drop handler is allowed to fill (see useDragDrop)
  droppable?: boolean;
  /// Node injected after a given field, e.g. the two-root verdict box under `target`
  after?: (key: string) => ReactNode;
  disabledField?: (key: string) => boolean;
  /// Lets the caller attach a section anchor so a config pill can scroll straight to a group
  groupRef?: (group: string, el: HTMLElement | null) => void;
  /// Body for a `custom` field — the junk-preset checkbox block, which edits `exclude` rather than
  /// holding a form value of its own
  renderCustom?: (f: FSpec) => ReactNode;
}

export function SchemaForm(props: SchemaFormProps) {
  const { fields, values, set, onPick, onSwap, pathClass, droppable, after, disabledField, groupRef, renderCustom } = props;

  return (
    <>
      {fields.map((f) => {
        const v = values[f.key];
        const disabled = disabledField?.(f.key) ?? false;
        const hint = f.hint ? <span className="thin">{f.hint}</span> : null;

        let control: ReactNode;
        if (f.kind === 'select') {
          control = (
            <>
              <span>{f.label}</span>
              <select value={String(v ?? '')} disabled={disabled} onChange={(e) => set(f.key, e.target.value)}>
                {f.opts!.map((o) => <option key={o} value={o}>{o}</option>)}
              </select>
            </>
          );
        } else if (f.kind === 'bool') {
          control = (
            <>
              <input type="checkbox" checked={!!v} disabled={disabled} onChange={(e) => set(f.key, e.target.checked)} />
              <span>{f.label}</span>
            </>
          );
        } else if (f.kind === 'lines') {
          control = (
            <>
              <span>{f.label}</span>
              <textarea
                spellCheck={false}
                value={String(v ?? '')}
                disabled={disabled}
                onChange={(e) => set(f.key, e.target.value)}
              />
            </>
          );
        } else if (f.kind === 'custom') {
          control = (
            <>
              <span>{f.label}</span>
              {renderCustom?.(f)}
            </>
          );
        } else if (f.kind === 'num') {
          control = (
            <>
              <span>{f.label}</span>
              <input
                type="number"
                step="any"
                value={String(v ?? '')}
                disabled={disabled}
                onChange={(e) => set(f.key, e.target.value)}
              />
            </>
          );
        } else if (f.kind === 'dir' || f.kind === 'file') {
          control = (
            <>
              <span>{f.label}</span>
              <div className="pathrow">
                <input
                  type="text"
                  className={pathClass?.(f.key) ?? ''}
                  data-drop={droppable ? '1' : undefined}
                  data-k={f.key}
                  list="sd-paths"
                  spellCheck={false}
                  value={String(v ?? '')}
                  disabled={disabled}
                  onChange={(e) => set(f.key, e.target.value)}
                />
                {f.key === 'source' && onSwap && (
                  <button type="button" className="pbtn" title="Swap with target" onClick={onSwap}>⇄</button>
                )}
                {onPick && (
                  <button
                    type="button"
                    className="pbtn"
                    title="Browse…"
                    onClick={() => onPick(f.key, f.kind as 'dir' | 'file')}
                  >📁</button>
                )}
              </div>
            </>
          );
        } else {
          control = (
            <>
              <span>{f.label}</span>
              <input
                type="text"
                spellCheck={false}
                value={String(v ?? '')}
                disabled={disabled}
                onChange={(e) => set(f.key, e.target.value)}
              />
            </>
          );
        }

        // A Fragment, not a wrapper element: the sheet is a two-column grid and any real box here would
        // become a single grid item, collapsing every field into one cell.
        // Every field is a <label> wrapping its one control — except `custom`, which holds a whole grid
        // of its own labelled checkboxes. Nesting labels is invalid HTML and makes a click ambiguous:
        // the browser may forward it to the outer label's control, i.e. the *first* checkbox in the
        // grid, so ticking "Developer" would silently tick "Windows" instead.
        const Wrap = f.kind === 'custom' ? 'div' : 'label';
        return (
          <Fragment key={f.key}>
            {f.group && (
              <div className="ed-group" ref={(el) => groupRef?.(f.group!, el)}>{f.group}</div>
            )}
            <Wrap className={'ed-field' + (f.wide ? ' wide' : '') + (f.kind === 'bool' ? ' ed-check' : '')}>
              {control}
              {hint}
            </Wrap>
            {after?.(f.key)}
          </Fragment>
        );
      })}
    </>
  );
}
