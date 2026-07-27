import { useEffect, useState } from 'react';
import { junkPresets } from '../../core/ipc';
import { excludeLines, presetState, togglePreset } from '../../core/junk';
import type { JunkPresetDto } from '../../core/ipc';

interface Props {
  presets: JunkPresetDto[];
  /// The job's exclude list, one pattern per line — the same textarea value the form holds
  excludeText: string;
  onChange: (next: string) => void;
}

export function useJunkPresets(): JunkPresetDto[] {
  const [presets, setPresets] = useState<JunkPresetDto[]>([]);
  useEffect(() => {
    let live = true;
    junkPresets().then((p) => { if (live) setPresets(p); }).catch(() => { /* the exclude box still works by hand */ });
    return () => { live = false; };
  }, []);
  return presets;
}

/// A preset is a macro over the exclude list below, never a parallel rule set: what the box says is
/// therefore exactly what the job excludes. The old shape — a job saying `os_excludes = "auto"` with an
/// empty exclude box — could not answer "what is actually excluded here".
export function JunkPresets({ presets, excludeText, onChange }: Props) {
  const lines = excludeLines(excludeText);

  if (!presets.length) return null;
  return (
    <div className="junkgrid">
      {presets.map((p) => {
        const { present, state } = presetState(p, lines);
        return (
          <label
            key={p.id}
            className="junkbox"
            title={`${p.hint}\n\n${p.patterns.join('\n')}${
              state === 'some' ? `\n\n(${present} of ${p.patterns.length} of these lines are currently in the exclude list)` : ''}`}
          >
            <input
              type="checkbox"
              checked={state === 'on'}
              // A partly-present preset clicks to "on", completing it — the useful move, and the one
              // the checked=false/indeterminate=true pair would otherwise make ambiguous
              ref={(el) => { if (el) el.indeterminate = state === 'some'; }}
              onChange={(e) => onChange(togglePreset(p, excludeText, e.target.checked || state === 'some'))}
            />
            <span className="jb-label">{p.label}</span>
            <span className="jb-count">{state === 'some' ? `${present}/${p.patterns.length}` : p.patterns.length}</span>
          </label>
        );
      })}
    </div>
  );
}
