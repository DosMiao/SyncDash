import {
  ArrowLeftRight,
  RefreshCw,
  ScrollText,
  Sigma,
  Timer,
} from 'lucide-react';
import { MODE_SUMMARIES, RIGOR_SUMMARIES } from '#core/domain/jobs/formSchema.ts';
import { humanSize } from '#core/shared/format.ts';
import { RESULT_TYPE_ICON } from '#ui/shared/icons/compareIcons.tsx';
import type { ReactNode } from 'react';
import type { JobDto } from '#core/types/generated/JobDto.ts';
import type { AutoScanStatusDto } from '#core/types/generated/AutoScanStatusDto.ts';
import { autoScanButtonLabel } from '#core/application/autoscan/autoscan.ts';
import type { SelectedRunStats } from '#ui/features/compare-results/model/selectedRunStats.ts';

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
  autoScanStatus: AutoScanStatusDto | null;
  autoScanControlPending: 'start' | 'stop' | null;
  onCompare: () => void;
  onSync: () => void;
  onToggleLog: () => void;
  onToggleAutoScan: () => void;
}

/// Zero values remain readable because absence is meaningful result data.
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
    autoScanStatus,
    autoScanControlPending,
    onCompare,
    onSync,
    onToggleLog,
    onToggleAutoScan,
  } = props;
  const autoScanActive = autoScanStatus?.active === true;
  const autoScanMode = autoScanActive ? autoScanStatus.mode : null;
  const autoScanLabel = autoScanButtonLabel(autoScanStatus, autoScanControlPending);

  const rigorSummary = job ? RIGOR_SUMMARIES[job.rigor] : undefined;
  const compareConfigurationLabel = job
    ? (rigorSummary ? `${job.rigor} · ${rigorSummary}` : job.rigor)
    : 'Select a job';

  return (
    <header className="toolbar">
      <div className="tb-run">
        <button
          type="button"
          className="btn primary"
          disabled={busy || !job}
          title={job ? `Walk both roots and build a plan (F5).\nRigor: ${compareConfigurationLabel}` : 'Select a job first'}
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

        {/* The button names the job mode because mirror and enrich are not synchronization modes. */}
        <button
          type="button"
          className="btn accent mode-btn"
          disabled={busy || !canSync}
          title={job
            ? `${job.mode}: ${MODE_SUMMARIES[job.mode] ?? ''}${job.versioning ? ' · versioning on' : ''}`
              + `${hasPlan
                ? canSync
                  ? `\nRuns ${executableCount} included differences (F9)`
                  : `\nUnavailable: ${applyBlockedMessage ?? 'review the current result'}`
                : '\nCompare first'}`
            : undefined}
          onClick={onSync}
        >{job ? job.mode.toUpperCase() : 'No job'}</button>

        {busy && <span className="spinner" />}
      </div>

      <div className="tb-side">
        <button type="button" className="btn" title="Show the run log" onClick={onToggleLog}>
          <ScrollText size={13} /> Log
        </button>

        <button
          type="button"
          className={'btn autoscan-btn' + (autoScanActive ? ' on-solid' : '')}
          title={!autoScanActive
            ? "Compare automatically while SyncDash is open"
            : autoScanMode === 'native_fsevents'
              ? `Watching '${autoScanStatus?.job_name ?? 'the monitored job'}' target ${(autoScanStatus?.target_index ?? 0) + 1} with FSEvents, with periodic full verification`
              : autoScanMode === 'polling'
                ? `Polling '${autoScanStatus?.job_name ?? 'the monitored job'}' target ${(autoScanStatus?.target_index ?? 0) + 1} every ${autoScanStatus?.interval_secs ?? '?'}s while SyncDash is open`
                : `Preparing backend-owned change detection for '${autoScanStatus?.job_name ?? 'the monitored job'}' target ${(autoScanStatus?.target_index ?? 0) + 1}`}
          aria-pressed={autoScanActive}
          aria-label={autoScanActive
            ? `Stop AutoScan for ${autoScanStatus?.job_name ?? 'the monitored job'}, target ${(autoScanStatus?.target_index ?? 0) + 1}`
            : 'Start AutoScan for the selected job and target'}
          disabled={autoScanControlPending !== null || (!autoScanActive && (!job || busy))}
          onClick={onToggleAutoScan}
        >
          <Timer size={13} />
          <span className="autoscan-label">{autoScanLabel}</span>
        </button>
      </div>
    </header>
  );
}
