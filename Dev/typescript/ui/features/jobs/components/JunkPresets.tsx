import { excludeLines, presetState, togglePreset } from '#core/domain/jobs/junk.ts';
import type { JunkPresetDto } from '#core/types/generated/JunkPresetDto.ts';

interface JunkPresetsProps {
  presets: JunkPresetDto[];
  excludeText: string;
  onChange: (next: string) => void;
}

export function JunkPresets({ presets, excludeText, onChange }: JunkPresetsProps) {
  const lines = excludeLines(excludeText);

  if (!presets.length) return null;
  return (
    <div className="junkgrid">
      {presets.map((preset) => {
        const { present, state } = presetState(preset, lines);
        return (
          <label
            key={preset.id}
            className="junkbox"
            title={`${preset.hint}\n\n${preset.patterns.join('\n')}${
              state === 'some' ? `\n\n(${present} of ${preset.patterns.length} of these lines are currently in the exclude list)` : ''}`}
          >
            <input
              type="checkbox"
              checked={state === 'on'}
              // A partial preset resolves to enabled because HTML exposes indeterminate separately from checked.
              ref={(checkbox) => { if (checkbox) checkbox.indeterminate = state === 'some'; }}
              onChange={(event) => onChange(togglePreset(
                preset,
                excludeText,
                event.target.checked || state === 'some',
              ))}
            />
            <span className="jb-label">{preset.label}</span>
            <span className="jb-count">
              {state === 'some' ? `${present}/${preset.patterns.length}` : preset.patterns.length}
            </span>
          </label>
        );
      })}
    </div>
  );
}
