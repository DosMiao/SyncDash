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
export interface FSpec {
  key: string;
  label: string;
  kind: FKind;
  opts?: string[];
  hint?: string;
  wide?: boolean;
  /// Starts a new titled section; the config pills on the main screen jump straight to one of these
  group?: string;
}

export const ED_FIELDS: FSpec[] = [
  { key: '__name', label: 'Job name (file name)', kind: 'text', group: 'Basics' },
  { key: 'mode', label: 'Mode', kind: 'select', opts: ['mirror', 'sync', 'enrich'] },
  { key: 'source', label: 'source root', kind: 'dir', wide: true },
  { key: 'target', label: 'target root', kind: 'dir', wide: true },
  { key: 'targets', label: 'Multiple targets (one root per line; overrides the single target above when non-empty)', kind: 'lines', wide: true, hint: '1:N: one source mirrored/enriched into several targets in turn, each with its own plan and logs. Not supported in sync mode (use paired jobs)' },
  { key: 'archive', label: 'Archive file (sync mode)', kind: 'file', hint: 'Empty = none; suggested %APPDATA%\\syncdash\\archives\\<name>.jsonl', wide: true },
  { key: 'rigor', label: 'Rigor (shortcut preset)', kind: 'select', opts: ['quick', 'fast', 'standard', 'paranoid', 'custom'], hint: 'A preset sets the four details below in one click; changing any of them by hand switches to custom. Ladder: each tier actually reads more this round' },
  { key: 'evidence', label: '· Content evidence', kind: 'select', opts: ['none', 'sampled', 'full'], hint: 'none = no reads, metadata only | sampled = size + 256 KB at head/middle/tail | full = whole-file BLAKE3' },
  { key: 'use_cache', label: '· Use the hash cache (trust last result for unchanged files, no real read)', kind: 'bool' },
  { key: 'escalate', label: '· Escalate on divergence (same digest but mtime differs >2 s → full re-verify on both sides)', kind: 'bool' },
  { key: 'verify_writes', label: '· Verify after write (full hash of the copy stream vs a re-read from disk)', kind: 'bool' },
  { key: 'symlinks', label: 'symlink policy', kind: 'select', opts: ['exclude', 'direct'] },
  { key: 'case_sensitive', label: 'Case-sensitive compare', kind: 'bool' },
  { key: 'versioning', label: 'Versioning (.version_syncDash)', kind: 'bool', group: 'Behavior' },
  { key: 'delta', label: 'Local delta writes (delta)', kind: 'bool' },
  { key: 'fsync', label: 'fsync before rename', kind: 'bool' },
  { key: 'sync_mode', label: 'Synchronize unix permission bits', kind: 'bool' },
  { key: 'parallel', label: 'Parallel width (empty = 4)', kind: 'num' },
  { key: 'on_conflict', label: 'Conflict policy', kind: 'select', opts: ['report', 'copy', 'newer'] },
  { key: 'max_conflicts', label: 'Conflict copy limit', kind: 'num' },
  { key: 'require_marker', label: 'Require a .syncdash-root marker', kind: 'bool', group: 'Guardrails' },
  { key: 'min_free_pct', label: 'Minimum free disk ratio (0.01 = 1%)', kind: 'num' },
  { key: 'max_delete_ratio', label: 'Delete ratio gate (0.5 = 50%)', kind: 'num' },
  { key: '__junk', label: 'Junk presets', kind: 'custom', wide: true, group: 'Filters', hint: 'Each preset is a macro over the exclude list below: ticking writes its patterns in verbatim, unticking takes exactly those lines back out. A tick adds nothing you cannot see, and nothing puts a line back once you delete it. The excluded count is stated in the status bar under "⚠ Excluded" — never silently' },
  { key: 'include', label: 'include (one per line)', kind: 'lines', wide: true },
  // Naming the one remaining unconditional rule rather than letting the hint above imply there is none.
  // It is four names, it is why the mount-point gate works (a synced .syncdash-root would grow on an
  // unmounted empty directory and defeat it), and a filter you cannot see is exactly what this screen
  // was rebuilt to stop having.
  { key: 'exclude', label: 'exclude (one per line; leading ! = exception)', kind: 'lines', wide: true, hint: 'This list is the filter. The only thing excluded without appearing here is SyncDash\'s own metadata — .syncdash-root, .syncdash.lock, its in-flight temp files and .version_syncDash — which cannot be synced without breaking the mount-point gate and the versioning store' },
  { key: 'deletable', label: 'deletable (may be removed along with a deleted parent directory)', kind: 'lines', wide: true },
  { key: 'remote_host', label: 'ssh host alias', kind: 'text', group: 'Remote (optional)' },
  { key: 'remote_root', label: 'Remote root path', kind: 'text' },
  { key: 'remote_exe', label: 'Remote syncdash path (empty = PATH)', kind: 'text' },
  { key: 'watch_interval_secs', label: 'Scheduled scan interval (seconds; empty = off)', kind: 'num', group: 'Watch', hint: 'Seconds = near real time; for UNC targets use ≥30' },
  { key: 'watch_auto_apply', label: 'Run automatically when differences are found', kind: 'bool' },
];

export const SET_FIELDS: FSpec[] = [
  { key: 'log_dir', label: 'Log directory (empty = default %APPDATA%\\syncdash\\logs)', kind: 'dir', wide: true, group: 'Location' },
  { key: 'level', label: 'Record level', kind: 'select', opts: ['info', 'warn', 'error'], hint: 'Narration below this level is not written to disk; the error list is unaffected', group: 'Content' },
  { key: 'log_compare', label: 'Compare runs', kind: 'select', opts: ['summary', 'off'], hint: 'summary = one summary line, no directory (Watch runs every 30 s, and creating a directory each time would flood the disk)' },
  { key: 'mirror_stderr', label: 'CLI also prints to the terminal', kind: 'bool' },
  { key: 'keep_days', label: 'Retention days (0 = no age-based cleanup)', kind: 'num', group: 'Retention' },
  { key: 'max_total_mb', label: 'Total size cap in MB (0 = unlimited)', kind: 'num', hint: 'The item list records everything — one big sync is tens of thousands of rows, and the total cap is its seatbelt' },
];

/// Optional fields where an empty string means "unset" rather than "the empty string" (serde Option)
export const NULLABLE_TEXT = new Set(['archive', 'remote_host', 'remote_root', 'remote_exe']);

/// Preset → the four detail knobs (aligned word for word with Rust config::rigor_resolved)
export const RIGOR_PRESETS: Record<string, { evidence: string; use_cache: boolean; escalate: boolean; verify_writes: boolean }> = {
  quick: { evidence: 'none', use_cache: false, escalate: false, verify_writes: false },
  fast: { evidence: 'sampled', use_cache: true, escalate: true, verify_writes: false },
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

/// One-to-one with the preset baselines in config::rigor_resolved (quick/fast/paranoid/everything else =
/// the standard baseline). Keep this in step when the ladder changes over there — the button subtitle is
/// the only place a user sees what a tier actually does.
export const RIGOR_HINT: Record<string, string> = {
  quick: 'size and time only',
  fast: 'sampled digest · uses cache',
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
export function formToJob(v: FormValues, base: JobFull): { name: string; job: JobFull } | { error: string } {
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
    if (f.key === '__name') { name = String(val ?? ''); continue; }
    j[f.key] = val;
  }
  if (!name) return { error: 'Job name cannot be empty' };
  const jf = j as unknown as JobFull;
  if (!jf.source || !jf.target) return { error: 'source / target cannot be empty' };
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
