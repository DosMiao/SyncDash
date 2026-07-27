// Junk exclude presets, on the frontend side.
//
// A preset is a **macro over `Job::exclude`**, never a parallel rule set: ticking one writes its patterns
// into the list verbatim, unticking takes exactly those lines back out. So there is no "which presets are
// on" state to keep anywhere — it is always derived from the list, and the list is the whole truth about
// what a job excludes.
//
// The pattern lists themselves are never duplicated here; they arrive from `junk_presets()`.

import type { JunkPresetDto } from './types/generated/JunkPresetDto';

/// Line identity for an exclude entry, matching Rust `filter::same_exclude_entry` step for step:
/// `fold(trim(s))` then backslash → slash, where `fold` is `text::norm_key(s, true)` = **NFC + uppercase**.
///
/// Both halves matter and neither is cosmetic. NFC: macOS/APFS writes NFD, so a path pasted from Finder
/// is a different string to a naive comparison than the same path typed on Windows — the engine folds
/// them together and the UI has to agree, or a checkbox reports "absent" for a line the filter is already
/// applying. Uppercase, not lowercase: the two are not inverses outside ASCII (ß uppercases to SS), and
/// this list holds real paths.
///
/// This is a **line identity** test, not mask matching. Whether two globs mean the same thing is not
/// guessed at here; mask semantics keep their single implementation in Rust (see `maskMatch`).
export function foldExcludeEntry(s: string): string {
  return s.trim().normalize('NFC').toUpperCase().replace(/\\/g, '/');
}

export function sameExcludeEntry(a: string, b: string): boolean {
  return foldExcludeEntry(a) === foldExcludeEntry(b);
}

/// The exclude textarea's raw text → the lines the filter would see
export function excludeLines(text: string): string[] {
  return text.split('\n').map((l) => l.trim()).filter(Boolean);
}

/// How much of a preset is present: all of it, none, or some. `some` is reachable — deleting one line of
/// a preset by hand is a reasonable thing to do — and it gets said out loud rather than rounded off.
export function presetState(p: JunkPresetDto, lines: string[]): { present: number; state: 'on' | 'off' | 'some' } {
  const have = new Set(lines.map(foldExcludeEntry));
  const present = p.patterns.filter((x) => have.has(foldExcludeEntry(x))).length;
  return { present, state: present === p.patterns.length ? 'on' : present === 0 ? 'off' : 'some' };
}

/// Ticking appends the patterns this list is missing; unticking drops exactly this preset's own lines.
/// Removal is only sound because the presets are disjoint, which the Rust side enforces with a test —
/// an overlap would let one box silently strip a rule another box still needs.
export function togglePreset(p: JunkPresetDto, text: string, on: boolean): string {
  const lines = excludeLines(text);
  if (on) {
    const have = new Set(lines.map(foldExcludeEntry));
    return [...lines, ...p.patterns.filter((x) => !have.has(foldExcludeEntry(x)))].join('\n');
  }
  const drop = new Set(p.patterns.map(foldExcludeEntry));
  return lines.filter((l) => !drop.has(foldExcludeEntry(l))).join('\n');
}

/// Append entries that are not already present, folded the way the engine folds them. Used by the row
/// context menu and by the funnel's "write to job exclude" — both of which now write into the same list
/// the presets live in, so a mask that happens to equal a preset pattern lights that preset's box up
/// rather than being pasted next to it.
export function addExcludeEntries(existing: string[], add: string[]): { next: string[]; added: string[] } {
  const next = [...existing];
  const added: string[] = [];
  for (const m of add) {
    if (next.some((e) => sameExcludeEntry(e, m))) continue;
    next.push(m);
    added.push(m);
  }
  return { next, added };
}

export interface PresetSummary {
  /// Presets whose every pattern is present
  on: string[];
  /// Preset label → "3/7", for presets a hand edit has left partly present
  partial: { label: string; present: number; total: number }[];
  /// Lines that belong to no preset
  custom: number;
}

/// What this job excludes, in one line's worth of structure. `exclude` is the whole policy now, so this
/// is an honest answer rather than a count of opaque strings.
export function summarizePresets(exclude: string[], presets: JunkPresetDto[]): PresetSummary {
  const on: string[] = [];
  const partial: { label: string; present: number; total: number }[] = [];
  const claimed = new Set<string>();
  for (const p of presets) {
    const { present, state } = presetState(p, exclude);
    if (state === 'on') on.push(p.label);
    else if (state === 'some') partial.push({ label: p.label, present, total: p.patterns.length });
    for (const pat of p.patterns) claimed.add(foldExcludeEntry(pat));
  }
  const custom = exclude.filter((e) => !claimed.has(foldExcludeEntry(e))).length;
  return { on, partial, custom };
}
