import { summarizePresets } from '#core/domain/jobs/junk.ts';
import type { Job as JobFull } from '#core/types/generated/Job.ts';
import type { JunkPresetDto } from '#core/types/generated/JunkPresetDto.ts';
import type { PeerLinkDto } from '#core/types/generated/PeerLinkDto.ts';

export interface Pill { key: string; value: string; group: string; title: string }

const formatPercentage = (value: number) => `${Math.round(value * 100)}%`;

/// `exclude` is the whole exclude policy, so this pill can answer "what does this job exclude" by
/// naming the presets rather than reporting a count of opaque strings. A preset only counts as on when
/// every one of its patterns is present; a partly-present one is called out as such rather than rounded.
function filterSummary(job: JobFull, presets: JunkPresetDto[]): string {
  if (!job.exclude.length) return 'nothing excluded';
  if (!presets.length) return `${job.exclude.length} excluded`;
  const summary = summarizePresets(job.exclude, presets);
  const parts = [...summary.on];
  for (const partial of summary.partial) {
    parts.push(`${partial.label} ${partial.present}/${partial.total}`);
  }
  if (summary.custom) parts.push(`${summary.custom} custom`);
  return parts.length ? parts.join(' · ') : `${job.exclude.length} excluded`;
}

/// Config overview on the main screen: only the settings that change the outcome; clicking a pill jumps
/// to the matching editor group. Data comes from get_job as a full Job — JobDto is left alone so we don't
/// contend with other changes over the same struct.
///
/// A pill states the setting and its value; what the value *means* is in the tooltip. These sit on
/// the main screen, where a parenthetical explanation costs a column of the diff table to say
/// something you only need once.
export function configPills(
  job: JobFull,
  peerLink: PeerLinkDto | null,
  presets: JunkPresetDto[],
): Pill[] {
  const pills: Pill[] = [
    {
      key: 'Filters',
      value: filterSummary(job, presets) + (job.include.length ? ` · ${job.include.length} allowed` : ''),
      group: 'Filters',
      title: 'Which junk presets are on, plus any hand-written rules. Whatever the filter removes is counted in the status bar.',
    },
    {
      key: 'Conflicts',
      value: job.on_conflict + (job.on_conflict === 'copy' ? ` ≤${job.max_conflicts}` : ''),
      group: 'Behavior',
      title: 'What happens when both sides changed since the last run.\nreport = list them and change nothing · copy = keep both sides · newer = the newer file wins',
    },
    {
      key: 'Versioning',
      value: job.versioning ? 'on' : 'off',
      group: 'Behavior',
      title: 'On: replaced and deleted files are kept under .version_syncDash in each root.\nOff: deletes go to the local trash instead.',
    },
    {
      key: 'Gates',
      value: `≤${formatPercentage(job.max_delete_ratio)} del · ≥${formatPercentage(job.min_free_pct)} free${job.require_marker ? ' · marker' : ''}`,
      group: 'Guardrails',
      title: `Review & Apply colors a category red once it touches more than ${formatPercentage(job.max_delete_ratio)} of the target. A run is blocked only if free disk is under ${formatPercentage(job.min_free_pct)}.`
        + (job.require_marker ? '\nBoth roots must also carry a .syncdash-root marker.' : ''),
    },
    {
      key: 'AutoScan',
      value: job.autoscan_interval_secs ? `${job.autoscan_interval_secs}s${job.autoscan_auto_apply ? ' · auto' : ''}` : 'off',
      group: 'AutoScan',
      title: job.autoscan_interval_secs
        ? `Compares every ${job.autoscan_interval_secs}s${job.autoscan_auto_apply ? ' and runs the result automatically' : ' and waits for you to review'}.`
        : 'No scheduled comparison.',
    },
  ];
  if (job.mode === 'sync') {
    pills.push({
      key: 'Archive',
      value: job.archive ? 'set' : 'none',
      group: 'Basics',
      title: job.archive
        ? `Last-run table: ${job.archive}`
        : 'Without an archive, sync mode cannot tell a delete from a file that was never there — deletes and moves are not attributed.',
    });
  }
  // The link used to live in three job fields; it is in the target phrase now, which the box above
  // already shows in full. So the pill reports only what the phrase does not make obvious: that
  // this job pushes to a peer, and whether it declared a mount to pull back through.
  //
  // Both facts arrive derived, from `run::peer_pull_mount` and the router's own peer test. The pill
  // used to read the phrase itself, with `startsWith('peer://')` and a `|mount=` match, and said
  // the opposite of what Apply would do for an upper-case scheme, a spaced `| mount = /x`, and an
  // empty `|mount=`.
  if (peerLink) {
    const mounted = peerLink.pull_mount !== null;
    pills.push({
      key: 'Peer',
      value: mounted ? 'push + pull' : 'push only',
      group: 'Basics',
      title: mounted
        ? 'The far side runs its own syncdash and applies what this side packs. Source-side (pull) ops write through the declared |mount= path.'
        : 'The far side runs its own syncdash and applies what this side packs. No |mount= is declared, so source-side (pull) ops are skipped — add |mount=<path serving the same tree> to enable them.',
    });
  }
  return pills;
}
