// Form schemas. The job editor and the log settings sheet are both generated from these lists, so a new
// knob is one line here rather than a hand-written label/input/collect triple in three places.

import type { AppSettings } from './types/generated/AppSettings';
import type { Job as JobFull } from './types/generated/Job';

/// What a rendered form holds. Deliberately not the domain shape: while you are typing a `lines` field,
/// "a\n" is not yet the array ["a"], and re-joining a split array on every keystroke eats the newline
/// under the cursor. Numbers stay strings for the same reason — an empty box is not 0.
export type FormValues = Record<string, string | boolean>;

/// 'custom' is a slot the caller fills (the junk-preset checkbox block); it carries no form value
export type FKind = 'text' | 'num' | 'bool' | 'select' | 'lines' | 'dir' | 'file' | 'custom';

/**
 * One row of a settings form.
 *
 * `label` / `desc` / `help` are three distinct jobs and used to be one field, which is how a
 * label ended up reading "Multiple targets (one root per line; overrides the single target above
 * when non-empty)". The renderer gives each its own typography, so the split is what makes the
 * hierarchy visible at all:
 *
 *   label  what this setting is          13px semibold  — always on screen
 *   desc   one short line, no more       11px muted     — always on screen
 *   help   the paragraph                 popover        — behind an info icon
 *
 * If a `desc` will not fit on one line, it belongs in `help`.
 */
export interface FSpec {
  key: string;
  label: string;
  kind: FKind;
  opts?: string[];
  desc?: string;
  help?: string;
  /// Key of the setting this one qualifies; rendered indented beneath it behind a rule
  parent?: string;
  /// Starts a new section; the section rail and the config pills on the main screen both key off this
  group?: string;
}

export const ED_FIELDS: FSpec[] = [
  { key: '__name', label: 'Job name', kind: 'text', group: 'Basics', desc: 'Also the name of the TOML file on disk.' },
  { key: 'mode', label: 'Mode', kind: 'select', opts: ['mirror', 'sync', 'enrich'], desc: 'mirror = source wins · sync = two-way · enrich = add only, never delete' },
  { key: 'source', label: 'Source root', kind: 'dir' },
  { key: 'target', label: 'Target root', kind: 'dir' },
  {
    key: 'targets', label: 'Multiple targets', kind: 'lines',
    desc: 'One root per line. Overrides the single target above when non-empty.',
    help: '1:N — one source mirrored or enriched into several targets in turn, each with its own plan and its own logs. Not supported in sync mode; use paired jobs there.',
  },
  { key: 'archive', label: 'Archive file', kind: 'file', desc: 'Sync mode only. Empty = none.', help: 'Suggested location: the archive/ directory beside your jobs/ one, as <name>.jsonl. syncdash gen-jobs fills this in for you.' },
  {
    key: 'rigor', label: 'Rigor', kind: 'select', opts: ['quick', 'fast', 'balanced', 'standard', 'paranoid', 'custom'],
    desc: 'A preset for the four knobs below. Changing one by hand switches this to custom.',
    help: 'quick = size and time only · fast = sampled digest with cache · balanced = cached comparison plus write verification · standard = fresh sampled evidence · paranoid = fresh full evidence.',
  },
  {
    key: 'evidence', label: 'Content evidence', kind: 'select', opts: ['none', 'sampled', 'full'], parent: 'rigor',
    desc: 'none = metadata only · sampled = size + 256 KB at head, middle and tail · full = whole-file BLAKE3',
  },
  { key: 'use_cache', label: 'Use the hash cache', kind: 'bool', parent: 'rigor', desc: 'Trust the last result for unchanged files instead of reading them again.' },
  { key: 'escalate', label: 'Escalate on divergence', kind: 'bool', parent: 'rigor', desc: 'Same digest but mtime differs by more than 2s — re-verify both sides in full.' },
  { key: 'verify_writes', label: 'Verify after write', kind: 'bool', parent: 'rigor', desc: 'Hash the copy stream and compare it against a re-read from disk.' },
  { key: 'symlinks', label: 'Symlink policy', kind: 'select', opts: ['exclude', 'direct'], desc: 'exclude = skip them · direct = copy the link itself, not its target' },
  { key: 'case_sensitive', label: 'Case-sensitive compare', kind: 'bool', desc: 'Off matches how NTFS and APFS behave by default.' },

  { key: 'versioning', label: 'Versioning', kind: 'bool', group: 'Behavior', desc: 'Keep replaced and deleted files under .version_syncDash in each root.' },
  { key: 'delta', label: 'Local delta writes', kind: 'bool', desc: 'Rewrite only the changed blocks of a large file.' },
  { key: 'fsync', label: 'fsync before rename', kind: 'bool', desc: 'Flush to disk before the atomic swap. Safer, slower.' },
  { key: 'sync_mode', label: 'Synchronize unix permission bits', kind: 'bool', desc: 'No effect when the target is a Windows filesystem.' },
  { key: 'parallel', label: 'Parallel width', kind: 'num', desc: 'Empty = 4.' },
  { key: 'on_conflict', label: 'Conflict policy', kind: 'select', opts: ['report', 'copy', 'newer'], desc: 'report = list them and change nothing · copy = keep both sides · newer = the newer file wins' },
  { key: 'max_conflicts', label: 'Conflict copy limit', kind: 'num', desc: 'How many conflicting files the copy policy will duplicate before stopping.' },

  { key: 'require_marker', label: 'Require a .syncdash-root marker', kind: 'bool', group: 'Guardrails', desc: 'Refuse to run unless both roots carry the marker file — catches an unmounted drive.' },
  { key: 'min_free_pct', label: 'Minimum free disk ratio', kind: 'num', desc: '0.01 = 1%. The run is blocked below this.' },
  { key: 'max_delete_ratio', label: 'Delete ratio gate', kind: 'num', desc: '0.5 = 50%. Blocks a run that would delete more than this share of the target.' },

  {
    key: '__junk', label: 'Junk presets', kind: 'custom', group: 'Filters',
    desc: 'Each preset writes its patterns straight into the exclude list below.',
    help: 'Ticking a preset writes its patterns in verbatim; unticking takes exactly those lines back out. A tick adds nothing you cannot see, and nothing puts a line back once you have deleted it. Whatever the filter removes is counted in the status bar — never silently.',
  },
  { key: 'include', label: 'Include', kind: 'lines', desc: 'One pattern per line. Empty = everything.' },
  // Naming the one remaining unconditional rule rather than letting the description imply there is
  // none. It is four names, it is why the mount-point gate works (a synced .syncdash-root would grow
  // on an unmounted empty directory and defeat it), and a filter you cannot see is exactly what this
  // screen was rebuilt to stop having.
  {
    key: 'exclude', label: 'Exclude', kind: 'lines',
    desc: 'One pattern per line; a leading ! makes an exception.',
    help: 'This list is the filter. The only thing excluded without appearing here is SyncDash\'s own metadata — .syncdash-root, .syncdash.lock, its in-flight temp files and .version_syncDash — none of which can be synced without breaking the mount-point gate and the versioning store.',
  },
  { key: 'deletable', label: 'Deletable', kind: 'lines', desc: 'May be removed along with a deleted parent directory.' },

  {
    key: 'watch_interval_secs', label: 'Maximum verification interval', kind: 'num', group: 'AutoScan',
    desc: 'Seconds. Local macOS roots also react to FSEvents; remote/unsupported roots poll at this interval while SyncDash is open.',
  },
  {
    key: 'watch_auto_apply', label: 'Run automatically when differences are found', kind: 'bool',
    help: 'Auto-run never grants permission to degraded capabilities or health warnings. If the exact job revision, target, capability set, and action set lack unattended authorization, AutoScan stops at review required.',
  },
];

export const SET_FIELDS: FSpec[] = [
  { key: 'log_dir', label: 'Log directory', kind: 'dir', group: 'Location', desc: 'Empty = %APPDATA%\\syncdash\\logs.' },
  { key: 'level', label: 'Record level', kind: 'select', opts: ['info', 'warn', 'error'], group: 'Content', desc: 'Narration below this level is not written to disk. The error list is unaffected.' },
  {
    key: 'log_compare', label: 'Compare runs', kind: 'select', opts: ['summary', 'off'],
    desc: 'summary = one line, no directory.',
    help: 'AutoScan compares every 30 s, and creating a run directory each time would flood the log disk.',
  },
  { key: 'mirror_stderr', label: 'CLI also prints to the terminal', kind: 'bool' },
  { key: 'keep_days', label: 'Retention days', kind: 'num', group: 'Retention', desc: '0 = no age-based cleanup.' },
  {
    key: 'max_total_mb', label: 'Total size cap in MB', kind: 'num', desc: '0 = unlimited.',
    help: 'The item list records every file touched — one big sync is tens of thousands of rows, and this cap is its seatbelt.',
  },
];

/// Section titles in rail order, derived from whichever fields open a group
export function groupsOf(fields: FSpec[]): string[] {
  return fields.map((f) => f.group).filter((g): g is string => !!g);
}

/// The fields of one section. `group` marks where a section *starts*, so a field belongs to the
/// most recent group above it — the list stays flat and a field only ever names its section when
/// it opens one.
export function fieldsInGroup(fields: FSpec[], group: string): FSpec[] {
  let cur = '';
  return fields.filter((f) => {
    if (f.group) cur = f.group;
    return cur === group;
  });
}

/// Optional fields where an empty string means "unset" rather than "the empty string" (serde Option)
export const NULLABLE_TEXT = new Set(['archive']);

/// Preset → the four detail knobs (aligned word for word with Rust config::rigor_resolved)
export const RIGOR_PRESETS: Record<string, { evidence: string; use_cache: boolean; escalate: boolean; verify_writes: boolean }> = {
  quick: { evidence: 'none', use_cache: false, escalate: false, verify_writes: false },
  fast: { evidence: 'sampled', use_cache: true, escalate: true, verify_writes: false },
  balanced: { evidence: 'sampled', use_cache: true, escalate: true, verify_writes: true },
  standard: { evidence: 'sampled', use_cache: false, escalate: true, verify_writes: true },
  paranoid: { evidence: 'full', use_cache: false, escalate: false, verify_writes: true },
};

/// Older job files may leave the details null (follow the preset) — the editor materializes them all
/// (and writes them explicitly on save)
export function applyRigorPresetDefaults(j: JobFull): JobFull {
  const p = RIGOR_PRESETS[j.rigor] ?? RIGOR_PRESETS.standard;
  return {
    ...j,
    evidence: j.evidence ?? p.evidence,
    use_cache: j.use_cache ?? p.use_cache,
    escalate: j.escalate ?? p.escalate,
    verify_writes: j.verify_writes ?? p.verify_writes,
  };
}

/// Which preset (if any) a set of four detail values corresponds to; 'custom' when it matches none
export function detectRigor(evidence: string, useCache: boolean, escalate: boolean, verifyWrites: boolean): string {
  const hit = Object.entries(RIGOR_PRESETS).find(([, p]) =>
    p.evidence === evidence && p.use_cache === useCache && p.escalate === escalate && p.verify_writes === verifyWrites);
  return hit ? hit[0] : 'custom';
}

/// One-to-one with the preset baselines in job::rigor. Keep this in step when the ladder changes there — the button subtitle is
/// the only place a user sees what a tier actually does.
export const RIGOR_HINT: Record<string, string> = {
  quick: 'size and time only',
  fast: 'sampled digest · uses cache',
  balanced: 'cached digest · verify after write',
  standard: 'sampled digest · no cache · escalate on divergence',
  paranoid: 'full hash · verify after write',
  custom: 'per the four detail knobs',
};

export const MODE_HINT: Record<string, string> = {
  mirror: 'source wins', sync: 'two-way', enrich: 'add only, never delete',
};

// Form values ↔ domain objects

function toFormValue(kind: FKind, v: unknown): string | boolean {
  if (kind === 'bool') return !!v;
  if (kind === 'lines') return ((v as string[]) ?? []).join('\n');
  return v == null ? '' : String(v);
}

export function jobToForm(j: JobFull, name: string): FormValues {
  const rec = j as unknown as Record<string, unknown>;
  const out: FormValues = {};
  for (const f of ED_FIELDS) {
    if (f.kind === 'custom') continue; // a rendered slot, not a value
    out[f.key] = toFormValue(f.kind, f.key === '__name' ? name : rec[f.key]);
  }
  return out;
}

/// Returns the job to save, or the reason it cannot be saved.
///
/// `base` is the job the form was opened on — the loaded job when editing, the engine's own default job
/// when creating. Fields the schema does not surface are carried through from it rather than reset to a
/// value invented here: `no_hash` is not in ED_FIELDS and it forces hashing off last in `rigor_resolved`,
/// so rebuilding the job from a blank default quietly cleared it every time anything else was saved.
export function formToJob(
  v: FormValues,
  base: JobFull,
): { name: string; job: JobFull } | { error: string; field: '__name' | 'source' | 'target' } {
  const j = { ...(base as unknown as Record<string, unknown>) };
  let name = '';
  for (const f of ED_FIELDS) {
    if (f.kind === 'custom') continue;
    const raw = v[f.key];
    let val: unknown;
    if (f.kind === 'bool') val = !!raw;
    else if (f.kind === 'num') val = String(raw).trim() === '' ? null : Number(raw);
    else if (f.kind === 'lines') val = String(raw).split('\n').map((s) => s.trim()).filter(Boolean);
    else {
      const s = String(raw).trim();
      val = s === '' && NULLABLE_TEXT.has(f.key) ? null : s;
    }
    if (f.key === '__name') { name = String(val ?? '').trim(); continue; }
    j[f.key] = val;
  }
  if (!name) return { error: 'Job name cannot be empty', field: '__name' };
  const jf = j as unknown as JobFull;
  if (!jf.source.trim()) return { error: 'Source root cannot be empty', field: 'source' };
  if (!jf.target.trim()) return { error: 'Target root cannot be empty', field: 'target' };
  return { name, job: jf };
}

export function settingsToForm(s: AppSettings): FormValues {
  const rec = s as unknown as Record<string, unknown>;
  const out: FormValues = {};
  for (const f of SET_FIELDS) out[f.key] = toFormValue(f.kind, rec[f.key]);
  return out;
}

export function formToSettings(v: FormValues): AppSettings {
  const out: Record<string, unknown> = {};
  for (const f of SET_FIELDS) {
    if (f.kind === 'bool') out[f.key] = !!v[f.key];
    else if (f.kind === 'num') out[f.key] = Number(String(v[f.key]).trim() || 0);
    else out[f.key] = String(v[f.key]).trim();
  }
  return out as unknown as AppSettings;
}

/// Our answer to the FFS "Save as batch job": the CLI already exists; all that was missing was handing
/// over the command. We do not register the system scheduled task for you — that is a system-settings
/// action, and a person should press it.
export function schtasksCmd(job: string): string {
  const exe = 'syncdash.exe';
  return `schtasks /create /tn "SyncDash-${job}" /tr "\\"${exe}\\" run ${job} --yes" /sc daily /st 22:00`;
}
