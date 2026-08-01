import {
  ArrowLeftRight,
  RefreshCw,
  ScrollText,
  Sigma,
  Timer,
} from 'lucide-react';
import { MODE_HINT, RIGOR_HINT } from '../../core/jobfields';
import { humanSize } from '../../core/format';
import { RESULT_TYPE_ICON } from '../icons';
import type { ReactNode } from 'react';
import type { JobDto } from '../../core/types/generated/JobDto';
import type { AutoScanStatusDto } from '../../core/types/generated/AutoScanStatusDto';
import { autoScanButtonLabel } from '../state/autoscan';

export interface SelectedRunStats {
  copyCount: number;
  updateCount: number;
  moveCount: number;
  deleteCount: number;
  transferBytes: number;
  reversedCount: number;
}

interface ToolbarProps {
  job: JobDto | null;
  hasPlan: boolean;
  executableCount: number;
  stats: SelectedRunStats | null;
  busy: boolean;
  canSync: boolean;
  applyBlockedMessage: string | null;
  /// Global backend monitor. It deliberately does not follow `job`: switching the selected view must
  /// not silently stop or relabel work that remains armed for another job.
  watchStatus: AutoScanStatusDto | null;
  watchPending: 'start' | 'stop' | null;
  onCompare: () => void;
  onSync: () => void;
  onToggleLog: () => void;
  onToggleWatch: () => void;
}

/// One count and its icon (same semantics as the FFS bottom bar). A zero recedes by color rather
/// than alpha — it still has to be readable to say "nothing of this kind", which is itself an answer.
function RunStat(props: { className?: string; icon: ReactNode; value: number | string; title: string }) {
  const { className, icon, value, title } = props;
  const zero = value === 0 || value === '0 B';
  return (
    <span className={'st' + (className ? ` ${className}` : '') + (zero ? ' zero' : '')} title={title}>
      {icon}<b>{value}</b>
    </span>
  );
}

export function Toolbar(props: ToolbarProps) {
  const {
    job,
    hasPlan,
    executableCount,
    stats,
    busy,
    canSync,
    applyBlockedMessage,
    watchStatus,
    watchPending,
    onCompare,
    onSync,
    onToggleLog,
    onToggleWatch,
  } = props;
  const watchActive = watchStatus?.active === true;
  const watchMode = watchActive ? watchStatus.mode : null;
  const watchLabel = autoScanButtonLabel(watchStatus, watchPending);

  // An unknown tier shows just its name, with no dangling "·" (the rigor ladder will gain tiers later)
  const rh = job ? RIGOR_HINT[job.rigor] : undefined;
  const cmpVariant = job ? (rh ? `${job.rigor} · ${rh}` : job.rigor) : 'Select a job';

  return (
    <header className="toolbar">
      {/* Left is the run, read straight across: press Compare, read what it found, press the mode
          button to carry it out. The counts sit *between* the two because they are the case for the
          second press — at the far edge they made you look away from the pair and back. Right is
          what runs alongside a run rather than being one: the log, and the scheduled scan.
          Editing the job is not on this bar at all — every setting worth changing is one click away
          on its own config pill below, which names the section it opens. */}
      <div className="tb-run">
        <button
          className="btn primary"
          disabled={busy || !job}
          title={job ? `Walk both roots and build a plan (F5).\nRigor: ${cmpVariant}` : 'Select a job first'}
          onClick={onCompare}
        ><RefreshCw size={13} /> Compare</button>

        {stats && (
          <span className="stats">
            <RunStat className="result-type-copy" icon={RESULT_TYPE_ICON.copy} value={stats.copyCount} title="Copy" />
            <RunStat className="result-type-update" icon={RESULT_TYPE_ICON.update} value={stats.updateCount} title="Update" />
            <RunStat className="result-type-move" icon={RESULT_TYPE_ICON.move} value={stats.moveCount} title="Move (No Re-transfer)" />
            <RunStat className="result-type-delete" icon={RESULT_TYPE_ICON.delete} value={stats.deleteCount} title="Delete (Into the Trash)" />
            <RunStat icon={<Sigma size={12} />} value={humanSize(stats.transferBytes) || '0 B'} title="Bytes to Transfer" />
            {stats.reversedCount > 0 && (
              <RunStat icon={<ArrowLeftRight size={12} />} value={stats.reversedCount} title="Reversed Direction" />
            )}
          </span>
        )}

        {/* The label is the **mode**, not a verb: what decides "what will happen" is mirror vs sync
            vs enrich, and the verb never changes. Writing "Synchronize" would also collide the
            action with the mode of the same name — and mirror is not synchronization at all. */}
        <button
          className="btn accent mode-btn"
          disabled={busy || !canSync}
          title={job
            ? `${job.mode}: ${MODE_HINT[job.mode] ?? ''}${job.versioning ? ' · versioning on' : ''}`
              + `${hasPlan
                ? canSync
                  ? `\nRuns ${executableCount} checked items (F9)`
                  : `\nUnavailable: ${applyBlockedMessage ?? 'review the current result'}`
                : '\nCompare first'}`
            : undefined}
          onClick={onSync}
        >{job ? job.mode.toUpperCase() : 'No job'}</button>

        {busy && <span className="spinner" />}
      </div>

      <div className="tb-side">
        <button className="btn" title="Show the run log" onClick={onToggleLog}>
          <ScrollText size={13} /> Log
        </button>

        {/* A solid fill rather than the usual tinted toggle: AutoScan is the one control here that
            keeps working after you look away, so it should be legible from across the room */}
        <button
          className={'btn autoscan-btn' + (watchActive ? ' on-solid' : '')}
          title={!watchActive
            ? "Compare automatically while SyncDash is open"
            : watchMode === 'native_fsevents'
              ? `Watching '${watchStatus?.job_name ?? 'the monitored job'}' target ${(watchStatus?.target_index ?? 0) + 1} with FSEvents, with periodic full verification`
              : watchMode === 'polling'
                ? `Polling '${watchStatus?.job_name ?? 'the monitored job'}' target ${(watchStatus?.target_index ?? 0) + 1} every ${watchStatus?.interval_secs ?? '?'}s while SyncDash is open`
                : `Preparing backend-owned change detection for '${watchStatus?.job_name ?? 'the monitored job'}' target ${(watchStatus?.target_index ?? 0) + 1}`}
          aria-pressed={watchActive}
          aria-label={watchActive
            ? `Stop AutoScan for ${watchStatus?.job_name ?? 'the monitored job'}, target ${(watchStatus?.target_index ?? 0) + 1}`
            : 'Start AutoScan for the selected job and target'}
          disabled={watchPending !== null || (!watchActive && (!job || busy))}
          onClick={onToggleWatch}
        >
          <Timer size={13} />
          <span className="autoscan-label">{watchLabel}</span>
        </button>
      </div>
    </header>
  );
}
