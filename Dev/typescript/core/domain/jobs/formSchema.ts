import { MTIME_SLACK_SECONDS } from '#core/domain/compare/plan.ts';
import { formatCount } from '#core/shared/format.ts';
import type { AppSettings } from '#core/types/generated/AppSettings.ts';
import type { Job as JobFull } from '#core/types/generated/Job.ts';
import type { SettingsNumericLimitsDto } from '#core/types/generated/SettingsNumericLimitsDto.ts';

/// Draft values preserve incomplete text and numbers exactly as typed; domain conversion happens
/// only at the save boundary.
export type FormValues = Record<string, string | boolean>;

export type FormFieldKind = 'text' | 'num' | 'bool' | 'select' | 'lines' | 'dir' | 'file' | 'custom';

/**
 * One row of a settings form.
 *
 * `label` names the setting, `desc` must fit on one line, and longer guidance belongs in `help`.
 */
export interface FormFieldSpec {
  key: string;
  label: string;
  kind: FormFieldKind;
  opts?: string[];
  desc?: string;
  help?: string;
  /// Key of the setting this field qualifies.
  parent?: string;
  /// Starts a section that continues until the next field with a group.
  group?: string;
}

export const JOB_FORM_FIELDS: FormFieldSpec[] = [
  { key: '__name', label: 'Job name', kind: 'text', group: 'Basics', desc: 'Also the name of the TOML file on disk.' },
  { key: 'mode', label: 'Mode', kind: 'select', opts: ['mirror', 'sync', 'enrich'], desc: 'mirror = source wins · sync = two-way · enrich = add only, never delete' },
  { key: 'source', label: 'Source root', kind: 'dir' },
  {
    key: 'targets', label: 'Target roots', kind: 'lines',
    desc: 'One root per line. At least one is required.',
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
  { key: 'escalate', label: 'Escalate on divergence', kind: 'bool', parent: 'rigor', desc: `Same digest but mtime differs by more than ${MTIME_SLACK_SECONDS}s — re-verify both sides in full.` },
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
  { key: 'max_delete_ratio', label: 'Delete ratio notice', kind: 'num', desc: '0.5 = 50%. A plan deleting more than this share is marked in Review & Apply; it never withholds a manual Apply. Only an unattended AutoScan auto-apply refuses on it.' },

  {
    key: '__junk', label: 'Junk presets', kind: 'custom', group: 'Filters',
    desc: 'Each preset writes its patterns straight into the exclude list below.',
    help: 'Ticking a preset writes its patterns in verbatim; unticking takes exactly those lines back out. A tick adds nothing you cannot see, and nothing puts a line back once you have deleted it. Whatever the filter removes is counted in the status bar — never silently.',
  },
  { key: 'include', label: 'Include', kind: 'lines', desc: 'One pattern per line. Empty = everything.' },
  {
    key: 'exclude', label: 'Exclude', kind: 'lines',
    desc: 'One pattern per line; a leading ! makes an exception.',
    help: 'This list is the filter. The only thing excluded without appearing here is SyncDash\'s own metadata — .syncdash-root, .syncdash.lock, its in-flight temp files and .version_syncDash — none of which can be synced without breaking the mount-point gate and the versioning store.',
  },
  { key: 'deletable', label: 'Deletable', kind: 'lines', desc: 'May be removed along with a deleted parent directory.' },

  {
    key: 'autoscan_interval_secs', label: 'Maximum verification interval', kind: 'num', group: 'AutoScan',
    desc: 'Seconds. Local macOS roots also react to FSEvents; VFS and unsupported roots poll at this interval while SyncDash is open.',
  },
  {
    key: 'autoscan_auto_apply', label: 'Run automatically when differences are found', kind: 'bool',
    help: 'Auto-run never grants permission to degraded capabilities or health warnings. If the exact job revision, target, capability set, and action set lack unattended authorization, AutoScan stops at review required.',
  },
];

export const SETTINGS_FORM_FIELDS: FormFieldSpec[] = [
  { key: 'log_dir', label: 'Log directory', kind: 'dir', group: 'Location', desc: 'Empty = SyncDash\'s default logs folder.' },
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

export function formGroupNames(fields: FormFieldSpec[]): string[] {
  return fields.map((f) => f.group).filter((g): g is string => !!g);
}

/// The fields of one section. `group` marks where a section *starts*, so a field belongs to the
/// most recent group above it — the list stays flat and a field only ever names its section when
/// it opens one.
export function formFieldsInGroup(fields: FormFieldSpec[], group: string): FormFieldSpec[] {
  let cur = '';
  return fields.filter((f) => {
    if (f.group) cur = f.group;
    return cur === group;
  });
}

const NULLABLE_JOB_TEXT_FIELDS = new Set(['archive']);

/// These values must remain identical to `Job::rigor_resolved` in `Dev/src/application/job/policy/execution.rs`.
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

export function detectRigor(evidence: string, useCache: boolean, escalate: boolean, verifyWrites: boolean): string {
  const hit = Object.entries(RIGOR_PRESETS).find(([, p]) =>
    p.evidence === evidence && p.use_cache === useCache && p.escalate === escalate && p.verify_writes === verifyWrites);
  return hit ? hit[0] : 'custom';
}

/// These labels describe the preset baselines in `Dev/src/application/job/rigor.rs`.
export const RIGOR_SUMMARIES: Record<string, string> = {
  quick: 'size and time only',
  fast: 'sampled digest · uses cache',
  balanced: 'cached digest · verify after write',
  standard: 'sampled digest · no cache · escalate on divergence',
  paranoid: 'full hash · verify after write',
  custom: 'per the four detail knobs',
};

export const MODE_SUMMARIES: Record<string, string> = {
  mirror: 'source wins', sync: 'two-way', enrich: 'add only, never delete',
};

function toFormValue(kind: FormFieldKind, v: unknown): string | boolean {
  if (kind === 'bool') return !!v;
  if (kind === 'lines') return ((v as string[]) ?? []).join('\n');
  return v == null ? '' : String(v);
}

export function jobToForm(j: JobFull, name: string): FormValues {
  const rec = j as unknown as Record<string, unknown>;
  const out: FormValues = {};
  for (const f of JOB_FORM_FIELDS) {
    if (f.kind === 'custom') continue;
    out[f.key] = toFormValue(f.kind, f.key === '__name' ? name : rec[f.key]);
  }
  return out;
}

/// Returns the job to save, or the reason it cannot be saved.
///
/// `base` is the job the form was opened on — the loaded job when editing, the engine's own default job
/// when creating. Fields the schema does not surface are carried through from it rather than reset to a
/// value invented here.
export function formToJob(
  v: FormValues,
  base: JobFull,
): { name: string; job: JobFull } | { error: string; field: '__name' | 'source' | 'targets' } {
  const j = { ...(base as unknown as Record<string, unknown>) };
  let name = '';
  for (const f of JOB_FORM_FIELDS) {
    if (f.kind === 'custom') continue;
    const raw = v[f.key];
    let val: unknown;
    if (f.kind === 'bool') val = !!raw;
    else if (f.kind === 'num') val = String(raw).trim() === '' ? null : Number(raw);
    else if (f.kind === 'lines') val = String(raw).split('\n').map((s) => s.trim()).filter(Boolean);
    else {
      const s = String(raw).trim();
      val = s === '' && NULLABLE_JOB_TEXT_FIELDS.has(f.key) ? null : s;
    }
    if (f.key === '__name') { name = String(val ?? '').trim(); continue; }
    j[f.key] = val;
  }
  if (!name) return { error: 'Job name cannot be empty', field: '__name' };
  const jf = j as unknown as JobFull;
  if (!jf.source.trim()) return { error: 'Source root cannot be empty', field: 'source' };
  if (jf.targets.length === 0) return { error: 'At least one target root is required', field: 'targets' };
  return { name, job: jf };
}

export function settingsToForm(s: AppSettings): FormValues {
  const rec = s as unknown as Record<string, unknown>;
  const out: FormValues = {};
  for (const f of SETTINGS_FORM_FIELDS) out[f.key] = toFormValue(f.kind, rec[f.key]);
  return out;
}

export type SettingsFormResult =
  | { settings: AppSettings }
  | { error: string; field: string };

export function formToSettings(
  v: FormValues,
  limits: SettingsNumericLimitsDto,
): SettingsFormResult {
  const out: Record<string, unknown> = {};
  for (const f of SETTINGS_FORM_FIELDS) {
    if (f.kind === 'bool') out[f.key] = !!v[f.key];
    else if (f.kind === 'num') {
      const raw = String(v[f.key]).trim();
      const value = raw === '' ? 0 : Number(raw);
      let maximum: number;
      if (f.key === 'keep_days') maximum = limits.maximum_keep_days;
      else if (f.key === 'max_total_mb') maximum = limits.maximum_total_mb;
      else return { error: `Numeric settings field '${f.key}' has no backend-owned limit`, field: f.key };
      if (!Number.isSafeInteger(value) || value < 0 || value > maximum) {
        return {
          error: `${f.label} must be a whole number from 0 through ${formatCount(maximum)}`,
          field: f.key,
        };
      }
      out[f.key] = value;
    }
    else out[f.key] = String(v[f.key]).trim();
  }
  return { settings: out as unknown as AppSettings };
}

export function windowsScheduledTaskCommand(job: string): string {
  const exe = 'syncdash.exe';
  return `schtasks /create /tn "SyncDash-${job}" /tr "\\"${exe}\\" run ${job} --yes" /sc daily /st 22:00`;
}
