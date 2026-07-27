import { humanSize } from '../../core/format';
import { MODE_HINT, RIGOR_HINT } from '../../core/jobfields';
import type { JobDto } from '../../core/types/generated/JobDto';

export interface PlanStats {
  copy: number;
  upd: number;
  mv: number;
  del: number;
  conflicts: number;
  bytes: number;
  flips: number;
}

interface Props {
  job: JobDto | null;
  hasPlan: boolean;
  /// Count of what would actually run (checked ∩ visible) — the same set the confirm sheet totals
  finalCount: number;
  stats: PlanStats | null;
  busy: boolean;
  canSync: boolean;
  watchSecs: number | null;
  onCompare: () => void;
  onSync: () => void;
  onEditGroup: (group: string) => void;
  onToggleLog: () => void;
  onToggleWatch: () => void;
}

/// M3: icon stats bar (same semantics as the FFS bottom bar: zeros dimmed, non-zeros bold)
function Seg({ cls, icon, n, title }: { cls?: string; icon: string; n: number | string; title: string }) {
  const zero = n === 0 || n === '0 B';
  return (
    <span className={'st' + (cls ? ' ' + cls : '') + (zero ? ' zero' : '')} title={title}>
      {icon}<b>{n}</b>
    </span>
  );
}

export function Toolbar(props: Props) {
  const { job, hasPlan, finalCount, stats, busy, canSync, watchSecs, onCompare, onSync, onEditGroup, onToggleLog, onToggleWatch } = props;

  // An unknown tier shows just its name, with no dangling "·" (the rigor ladder will gain tiers later)
  const rh = job ? RIGOR_HINT[job.rigor] : undefined;
  const cmpVariant = job ? (rh ? `${job.rigor} · ${rh}` : job.rigor) : 'Select a job';

  return (
    <header id="toolbar">
      {/* Each primary button and its settings cog are grouped so a toolbar wrap moves them together */}
      <div className="tgroup">
        <button className="btn primary two" disabled={busy || !job} onClick={onCompare}>
          <span className="l1">⟳ Compare</span>
          <span className="l2">{cmpVariant}</span>
        </button>
        <button
          className="btn cog"
          title={job ? `Compare settings: rigor ${job.rigor} / case sensitivity / symlinks` : 'Compare settings'}
          disabled={!job}
          onClick={() => onEditGroup('Basics')}
        >⚙</button>
      </div>

      {/* Information hierarchy on the run button: the large text is the **mode**, the small text is the
          action. Two reasons: (1) what decides "what will happen" is the mode, not the verb that never
          changes; (2) we already have a mode called sync, so writing Synchronize on the button collides
          the action with the mode — and mirror is not synchronization at all, it is one-way mirroring. */}
      <div className="tgroup">
        <button
          className="btn accent two mode-btn"
          disabled={busy || !canSync}
          title={job ? `${job.mode}: ${MODE_HINT[job.mode] ?? ''}${job.versioning ? ' · versioning on' : ''}` : undefined}
          onClick={onSync}
        >
          <span className={'l1 m-' + (job ? job.mode : 'none')}>{job ? job.mode.toUpperCase() : 'No job'}</span>
          <span className="l2">
            {job ? `Run${hasPlan ? ` ${finalCount} items` : ''} · ${MODE_HINT[job.mode] ?? ''}` : 'Compare first'}
          </span>
        </button>
        <button
          className="btn cog"
          title="Mode / conflict policy / versioning"
          disabled={!job}
          onClick={() => onEditGroup('Behavior')}
        >⚙</button>
      </div>

      <button className="btn cog" title="Run log" onClick={onToggleLog}>🗒</button>
      <button
        className={'btn' + (watchSecs !== null ? ' on' : '')}
        id="btn-watch"
        title="Watch: compare on the job's watch interval"
        disabled={!job}
        onClick={onToggleWatch}
      >{watchSecs !== null ? `⏱ Watch ${watchSecs}s` : '⏱ Watch'}</button>

      <span className="stats">
        {stats && (
          <>
            <Seg cls="s-copy" icon="✚" n={stats.copy} title="Copy" />
            <Seg cls="s-upd" icon="✎" n={stats.upd} title="Update" />
            <Seg cls="s-mv" icon="⇢" n={stats.mv} title="Move (no re-transfer)" />
            <Seg cls="s-del" icon="✕" n={stats.del} title="Delete (into the trash)" />
            <Seg cls="s-conf" icon="⚡" n={stats.conflicts} title="Conflict" />
            <Seg icon="Σ" n={humanSize(stats.bytes) || '0 B'} title="Bytes to transfer" />
            {stats.flips > 0 && <Seg icon="⇄" n={stats.flips} title="Reversed direction" />}
          </>
        )}
      </span>
      {busy && <span className="spinner" />}
    </header>
  );
}
